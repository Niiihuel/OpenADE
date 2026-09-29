//! Frozen MCP tool bindings composed with the local tool executor.
//!
//! Discovery is explicit and occurs before a runner turn. The resulting
//! `definitions()` never changes, even if a server later changes its catalog.
//! The caller must configure `PermissionPolicy` for remote effects. The
//! current `ToolCatalog` compiles object-root Draft 2020-12 schemas offline
//! and rejects invalid schemas or arguments before dispatch. A live MCP
//! `tools/list_changed` notification invalidates that server's frozen tools
//! for the remainder of the turn. The next turn rediscovers them.

use crate::{
    AgentError, McpCallResult, McpError, McpStdioClient, McpTool, ToolCatalog, ToolCatalogError,
    ToolContext, ToolDefinition, ToolExecutor, ToolOutput, MAX_ADVERTISED_TOOLS,
    MAX_TOOL_NAME_BYTES,
};
use async_trait::async_trait;
use openide_core::ToolCall;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const MAX_MCP_DESCRIPTION_BYTES: usize = 8 * 1024;

/// Minimal client boundary for tool discovery and invocation. Production uses
/// `McpStdioClient` or `McpHttpClient`; independent implementations can
/// exercise the adapter.
#[async_trait]
pub trait McpToolClient: Send + Sync {
    async fn list_tools(&self, cancellation: &CancellationToken) -> Result<Vec<McpTool>, McpError>;

    /// Sticky invalidation independent of notification delivery. Implementors
    /// without a live catalog signal retain the frozen snapshot behavior.
    fn catalog_changed(&self) -> bool {
        false
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        cancellation: &CancellationToken,
    ) -> Result<McpCallResult, McpError>;
}

#[async_trait]
impl McpToolClient for McpStdioClient {
    fn catalog_changed(&self) -> bool {
        McpStdioClient::catalog_changed(self)
    }

    async fn list_tools(&self, cancellation: &CancellationToken) -> Result<Vec<McpTool>, McpError> {
        McpStdioClient::list_tools(self, cancellation).await
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        cancellation: &CancellationToken,
    ) -> Result<McpCallResult, McpError> {
        McpStdioClient::call_tool(self, name, arguments, cancellation).await
    }
}

#[derive(Debug, Error)]
pub enum McpToolAdapterError {
    #[error("MCP server name cannot form a stable namespace: {0:?}")]
    InvalidServerName(String),
    #[error("duplicate MCP server name: {0}")]
    DuplicateServerName(String),
    #[error("MCP tool name must be nonempty on server {server}: {tool:?}")]
    InvalidToolName { server: String, tool: String },
    #[error("MCP tool description exceeds {MAX_MCP_DESCRIPTION_BYTES} bytes: {server}/{tool}")]
    DescriptionTooLong { server: String, tool: String },
    #[error("namespaced MCP tool name exceeds {MAX_TOOL_NAME_BYTES} bytes: {server}/{tool}")]
    NameTooLong { server: String, tool: String },
    #[error("duplicate exposed tool name: {0}")]
    Collision(String),
    #[error("MCP discovery failed for {server}: {source}")]
    Discovery {
        server: String,
        #[source]
        source: McpError,
    },
    #[error(transparent)]
    Catalog(#[from] ToolCatalogError),
}

/// One server and the exact tool list discovered from it. Construct through
/// `discover` so the client has also recorded its advertised names.
pub struct McpServerBinding {
    name: String,
    client: Arc<dyn McpToolClient>,
    tools: Vec<McpTool>,
}

impl McpServerBinding {
    pub async fn discover(
        name: impl Into<String>,
        client: Arc<dyn McpToolClient>,
        cancellation: &CancellationToken,
    ) -> Result<Self, McpToolAdapterError> {
        let name = name.into();
        if name.is_empty()
            || name.len() > MAX_TOOL_NAME_BYTES
            || encode_segment(&name).len() > MAX_TOOL_NAME_BYTES - "mcp____".len() - 1
        {
            return Err(McpToolAdapterError::InvalidServerName(name));
        }
        if cancellation.is_cancelled() {
            return Err(McpToolAdapterError::Discovery {
                server: name,
                source: McpError::Cancelled,
            });
        }
        let tools = client.list_tools(cancellation).await.map_err(|source| {
            McpToolAdapterError::Discovery {
                server: name.clone(),
                source,
            }
        })?;
        Ok(Self {
            name,
            client,
            tools,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

enum Route {
    Local,
    Mcp {
        server: String,
        tool: String,
        client: Arc<dyn McpToolClient>,
    },
}

/// ToolExecutor with one immutable catalog of local and discovered MCP tools.
/// Names are `mcp__<server>__<tool>`; bytes outside the local name alphabet
/// are encoded as `_xHH`. Collisions and overlong names fail construction.
pub struct McpCompositeTools {
    local: Arc<dyn ToolExecutor>,
    catalog: ToolCatalog,
    routes: BTreeMap<String, Route>,
    invalidated_servers: Mutex<BTreeSet<String>>,
}

impl McpCompositeTools {
    pub fn new(
        local: Arc<dyn ToolExecutor>,
        servers: Vec<McpServerBinding>,
    ) -> Result<Self, McpToolAdapterError> {
        let mut definitions = Vec::new();
        let mut routes = BTreeMap::new();
        for definition in local.definitions() {
            if definitions.len() >= MAX_ADVERTISED_TOOLS {
                return Err(McpToolAdapterError::Catalog(
                    ToolCatalogError::TooManyTools {
                        limit: MAX_ADVERTISED_TOOLS,
                    },
                ));
            }
            if routes
                .insert(definition.name.clone(), Route::Local)
                .is_some()
            {
                return Err(McpToolAdapterError::Collision(definition.name));
            }
            definitions.push(definition);
        }
        let mut server_names = BTreeSet::new();
        for binding in servers {
            if !server_names.insert(binding.name.clone()) {
                return Err(McpToolAdapterError::DuplicateServerName(binding.name));
            }
            let server_segment = encode_segment(&binding.name);
            for tool in binding.tools {
                if definitions.len() >= MAX_ADVERTISED_TOOLS {
                    return Err(McpToolAdapterError::Catalog(
                        ToolCatalogError::TooManyTools {
                            limit: MAX_ADVERTISED_TOOLS,
                        },
                    ));
                }
                if tool.name.is_empty() {
                    return Err(McpToolAdapterError::InvalidToolName {
                        server: binding.name,
                        tool: tool.name,
                    });
                }
                if tool
                    .description
                    .as_ref()
                    .is_some_and(|description| description.len() > MAX_MCP_DESCRIPTION_BYTES)
                {
                    return Err(McpToolAdapterError::DescriptionTooLong {
                        server: binding.name,
                        tool: tool.name,
                    });
                }
                if tool.name.len() > MAX_TOOL_NAME_BYTES {
                    return Err(McpToolAdapterError::NameTooLong {
                        server: binding.name,
                        tool: tool.name,
                    });
                }
                let exposed_name =
                    format!("mcp__{}__{}", server_segment, encode_segment(&tool.name));
                if exposed_name.len() > MAX_TOOL_NAME_BYTES {
                    return Err(McpToolAdapterError::NameTooLong {
                        server: binding.name,
                        tool: tool.name,
                    });
                }
                let description = match tool.description.as_deref() {
                    Some(description) if !description.is_empty() => {
                        format!("MCP server {}: {description}", binding.name)
                    }
                    _ => format!("MCP tool {} from server {}.", tool.name, binding.name),
                };
                let definition = ToolDefinition {
                    name: exposed_name.clone(),
                    description,
                    parameters: tool.input_schema,
                };
                let route = Route::Mcp {
                    server: binding.name.clone(),
                    tool: tool.name,
                    client: Arc::clone(&binding.client),
                };
                if routes.insert(exposed_name.clone(), route).is_some() {
                    return Err(McpToolAdapterError::Collision(exposed_name));
                }
                definitions.push(definition);
            }
        }
        let catalog = ToolCatalog::new(definitions)?;
        Ok(Self {
            local,
            catalog,
            routes,
            invalidated_servers: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    /// Exact names to configure in `PermissionPolicy` before exposing this
    /// executor to a runner. Local tools are excluded.
    pub fn mcp_tool_names(&self) -> impl Iterator<Item = &str> + '_ {
        self.routes
            .iter()
            .filter_map(|(name, route)| matches!(route, Route::Mcp { .. }).then_some(name.as_str()))
    }

    /// A server changed its tool list after this immutable catalog was sent
    /// to the model. Refuse future calls to that server until a new turn
    /// performs discovery, without altering the current catalog digest.
    pub fn invalidate_server_catalog(&self, server: &str) -> bool {
        let known = self
            .routes
            .values()
            .any(|route| matches!(route, Route::Mcp { server: name, .. } if name == server));
        if known {
            self.invalidated_servers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(server.to_owned());
        }
        known
    }

    pub fn is_server_catalog_invalidated(&self, server: &str) -> bool {
        if self.routes.values().any(|route| {
            matches!(route, Route::Mcp { server: name, client, .. } if name == server && client.catalog_changed())
        }) {
            self.invalidate_server_catalog(server);
        }
        self.invalidated_servers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(server)
    }
}

#[async_trait]
impl ToolExecutor for McpCompositeTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.catalog.definitions().to_vec()
    }

    fn certifies_parallel_read(&self, name: &str) -> bool {
        match self.routes.get(name) {
            Some(Route::Local) => self.local.certifies_parallel_read(name),
            Some(Route::Mcp { .. }) | None => false,
        }
    }

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if let Err(error) = self.catalog.require_valid_call(call) {
            return Ok(ToolOutput::error(error.to_string()));
        }
        match self.routes.get(&call.name) {
            Some(Route::Local) => self.local.execute(call, context).await,
            Some(Route::Mcp {
                server,
                tool,
                client,
            }) => {
                if self.is_server_catalog_invalidated(server) {
                    return Ok(ToolOutput::error(format!(
                        "MCP {server} changed its tool catalog; start a new turn to rediscover it"
                    )));
                }
                let result = client
                    .call_tool(tool, call.arguments.clone(), &context.cancel)
                    .await
                    .map_err(|error| match error {
                        McpError::EffectUncertain(message) => {
                            AgentError::EffectUncertain(format!("MCP {server}/{tool}: {message}"))
                        }
                        McpError::Remote { code, message } => AgentError::EffectUncertain(format!(
                            "MCP {server}/{tool}: remote error {code}: {message}"
                        )),
                        McpError::Cancelled => AgentError::Cancelled,
                        other => AgentError::Tool(format!("MCP {server}/{tool}: {other}")),
                    })?;
                let content = format_mcp_result(&result);
                Ok(ToolOutput {
                    content,
                    is_error: result.is_error,
                })
            }
            None => Ok(ToolOutput::error(format!("unknown tool: {}", call.name))),
        }
    }
}

fn encode_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
            encoded.push(char::from(byte));
        } else {
            // This is a byte encoding, so UTF-8 input is also deterministic.
            write!(encoded, "_x{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

fn format_mcp_result(result: &McpCallResult) -> String {
    let plain_text = result.structured_content.is_none()
        && result.extra.is_empty()
        && result.content.iter().all(|block| {
            block.as_object().is_some_and(|object| {
                object.len() == 2
                    && object.get("type").and_then(Value::as_str) == Some("text")
                    && object.get("text").and_then(Value::as_str).is_some()
            })
        });
    if plain_text {
        result
            .content
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        serde_json::to_string(result).expect("McpCallResult always serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalTools;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    struct MockClient {
        tools: Mutex<Vec<McpTool>>,
        calls: Mutex<Vec<(String, Value)>>,
        failure: bool,
        uncertain: bool,
        tool_error: bool,
        catalog_changed: AtomicBool,
    }

    impl MockClient {
        fn new(tools: Vec<McpTool>) -> Self {
            Self {
                tools: Mutex::new(tools),
                calls: Mutex::new(Vec::new()),
                failure: false,
                uncertain: false,
                tool_error: false,
                catalog_changed: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl McpToolClient for MockClient {
        fn catalog_changed(&self) -> bool {
            self.catalog_changed.load(Ordering::Acquire)
        }

        async fn list_tools(
            &self,
            _cancellation: &CancellationToken,
        ) -> Result<Vec<McpTool>, McpError> {
            Ok(self.tools.lock().unwrap().clone())
        }

        async fn call_tool(
            &self,
            name: &str,
            arguments: Value,
            _cancellation: &CancellationToken,
        ) -> Result<McpCallResult, McpError> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_owned(), arguments));
            if self.failure {
                return Err(McpError::Remote {
                    code: -32000,
                    message: "server failed".to_owned(),
                });
            }
            if self.uncertain {
                return Err(McpError::EffectUncertain("connection lost".to_owned()));
            }
            Ok(McpCallResult {
                content: vec![json!({"type":"text","text":"done"})],
                is_error: self.tool_error,
                structured_content: None,
                extra: BTreeMap::new(),
            })
        }
    }

    fn tool(name: &str) -> McpTool {
        McpTool {
            name: name.to_owned(),
            description: Some("A test MCP tool".to_owned()),
            input_schema: json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}),
            extra: BTreeMap::new(),
        }
    }

    fn context() -> ToolContext {
        ToolContext {
            workspace: std::env::temp_dir(),
            cancel: CancellationToken::new(),
        }
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "call-1".to_owned(),
            name: name.to_owned(),
            arguments: json!({"value":"hello"}),
        }
    }

    #[tokio::test]
    async fn freezes_local_and_mcp_catalog_and_routes_original_name() {
        let mock = Arc::new(MockClient::new(vec![tool("git.status")]));
        let binding = McpServerBinding::discover("source", mock.clone(), &CancellationToken::new())
            .await
            .unwrap();
        let composite = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]).unwrap();
        let name = "mcp__source__git_x2Estatus";
        assert_eq!(composite.definitions().len(), 4);
        assert!(composite.catalog().get(name).is_some());
        assert_eq!(composite.mcp_tool_names().collect::<Vec<_>>(), vec![name]);
        mock.tools.lock().unwrap().clear();
        assert_eq!(composite.definitions().len(), 4);
        let result = composite.execute(&call(name), &context()).await.unwrap();
        assert_eq!(result.content, "done");
        assert!(!result.is_error);
        assert_eq!(
            mock.calls.lock().unwrap().as_slice(),
            &[("git.status".to_owned(), json!({"value":"hello"}))]
        );
    }

    #[tokio::test]
    async fn list_change_refuses_only_affected_server_without_dispatch() {
        let changed = Arc::new(MockClient::new(vec![tool("first")]));
        let stable = Arc::new(MockClient::new(vec![tool("second")]));
        let first =
            McpServerBinding::discover("changed", changed.clone(), &CancellationToken::new())
                .await
                .unwrap();
        let second =
            McpServerBinding::discover("stable", stable.clone(), &CancellationToken::new())
                .await
                .unwrap();
        let composite =
            McpCompositeTools::new(Arc::new(LocalTools::new()), vec![first, second]).unwrap();
        let frozen_digest = composite.catalog().digest_hex();
        assert!(!composite.invalidate_server_catalog("missing"));
        assert!(composite.invalidate_server_catalog("changed"));
        let denied = composite
            .execute(&call("mcp__changed__first"), &context())
            .await
            .unwrap();
        assert!(denied.is_error);
        assert!(denied.content.contains("start a new turn"));
        assert!(changed.calls.lock().unwrap().is_empty());
        let allowed = composite
            .execute(&call("mcp__stable__second"), &context())
            .await
            .unwrap();
        assert!(!allowed.is_error);
        assert_eq!(stable.calls.lock().unwrap().len(), 1);
        assert_eq!(composite.catalog().digest_hex(), frozen_digest);
    }

    #[tokio::test]
    async fn sticky_client_flag_blocks_dispatch_before_runtime_listener_runs() {
        let client = Arc::new(MockClient::new(vec![tool("mutate")]));
        let binding =
            McpServerBinding::discover("changed", client.clone(), &CancellationToken::new())
                .await
                .unwrap();
        let composite = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]).unwrap();
        client.catalog_changed.store(true, Ordering::Release);
        let output = composite
            .execute(&call("mcp__changed__mutate"), &context())
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(client.calls.lock().unwrap().is_empty());
        assert!(composite.is_server_catalog_invalidated("changed"));
    }

    #[tokio::test]
    async fn rejects_collisions_across_encoded_namespace_and_local_tools() {
        let first = Arc::new(MockClient::new(vec![tool("c")]));
        let second = Arc::new(MockClient::new(vec![tool("b__c")]));
        let first = McpServerBinding::discover("a__b", first, &CancellationToken::new())
            .await
            .unwrap();
        let second = McpServerBinding::discover("a", second, &CancellationToken::new())
            .await
            .unwrap();
        let error = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![first, second]);
        assert!(
            matches!(error, Err(McpToolAdapterError::Collision(name)) if name == "mcp__a__b__c")
        );

        struct CollisionLocal;
        #[async_trait]
        impl ToolExecutor for CollisionLocal {
            fn definitions(&self) -> Vec<ToolDefinition> {
                vec![ToolDefinition {
                    name: "mcp__a__c".into(),
                    description: "local".into(),
                    parameters: json!({"type":"object"}),
                }]
            }
            async fn execute(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, AgentError> {
                Ok(ToolOutput::success("local"))
            }
        }
        let mock = Arc::new(MockClient::new(vec![tool("c")]));
        let binding = McpServerBinding::discover("a", mock, &CancellationToken::new())
            .await
            .unwrap();
        let error = McpCompositeTools::new(Arc::new(CollisionLocal), vec![binding]);
        assert!(matches!(error, Err(McpToolAdapterError::Collision(name)) if name == "mcp__a__c"));

        let encoding = Arc::new(MockClient::new(vec![tool("a.b"), tool("a_x2Eb")]));
        let binding = McpServerBinding::discover("server", encoding, &CancellationToken::new())
            .await
            .unwrap();
        let error = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]);
        assert!(
            matches!(error, Err(McpToolAdapterError::Collision(name)) if name == "mcp__server__a_x2Eb")
        );
    }

    #[tokio::test]
    async fn rejects_invalid_arguments_and_unadvertised_calls_before_dispatch() {
        let mock = Arc::new(MockClient::new(vec![tool("echo")]));
        let binding = McpServerBinding::discover("tools", mock.clone(), &CancellationToken::new())
            .await
            .unwrap();
        let composite = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]).unwrap();
        let denied = composite
            .execute(&call("mcp__tools__hidden"), &context())
            .await
            .unwrap();
        assert!(denied.is_error);
        let mut invalid = call("mcp__tools__echo");
        invalid.arguments = json!({"value":17});
        let denied = composite.execute(&invalid, &context()).await.unwrap();
        assert!(denied.is_error);
        assert!(mock.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn local_json_schema_ref_accepts_valid_args_and_rejects_invalid_args() {
        let mut definition = tool("restricted");
        definition.input_schema = json!({"type":"object","properties":{"value":{"$ref":"#/$defs/value"}},"$defs":{"value":{"type":"string"}}});
        let mock = Arc::new(MockClient::new(vec![definition]));
        let binding = McpServerBinding::discover("remote", mock.clone(), &CancellationToken::new())
            .await
            .unwrap();
        let composite = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]).unwrap();
        let valid = composite
            .execute(&call("mcp__remote__restricted"), &context())
            .await
            .unwrap();
        assert!(!valid.is_error);
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
        let mut invalid = call("mcp__remote__restricted");
        invalid.arguments = json!({"value":17});
        let denied = composite.execute(&invalid, &context()).await.unwrap();
        assert!(denied.is_error);
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rejects_invalid_server_name_duplicate_servers_and_overlong_tool_name() {
        let mock = Arc::new(MockClient::new(vec![tool("x")]));
        let invalid = McpServerBinding::discover("", mock.clone(), &CancellationToken::new()).await;
        assert!(matches!(
            invalid,
            Err(McpToolAdapterError::InvalidServerName(_))
        ));

        let first = McpServerBinding::discover("same", mock.clone(), &CancellationToken::new())
            .await
            .unwrap();
        let second = McpServerBinding::discover("same", mock.clone(), &CancellationToken::new())
            .await
            .unwrap();
        let duplicate = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![first, second]);
        assert!(
            matches!(duplicate, Err(McpToolAdapterError::DuplicateServerName(name)) if name == "same")
        );

        let long = Arc::new(MockClient::new(vec![tool(&"x".repeat(64))]));
        let binding = McpServerBinding::discover("server", long, &CancellationToken::new())
            .await
            .unwrap();
        let result = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]);
        assert!(matches!(
            result,
            Err(McpToolAdapterError::NameTooLong { .. })
        ));
    }

    #[tokio::test]
    async fn remote_errors_are_uncertain_and_cancellation_prevents_dispatch() {
        let mut failing = MockClient::new(vec![tool("fail")]);
        failing.failure = true;
        let mock = Arc::new(failing);
        let binding = McpServerBinding::discover("remote", mock.clone(), &CancellationToken::new())
            .await
            .unwrap();
        let composite = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]).unwrap();
        let error = composite
            .execute(&call("mcp__remote__fail"), &context())
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentError::EffectUncertain(message) if message.contains("server failed"))
        );
        let cancelled = context();
        cancelled.cancel.cancel();
        let error = composite
            .execute(&call("mcp__remote__fail"), &cancelled)
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::Cancelled));
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn local_parallel_read_certificate_survives_mcp_composition() {
        let binding = McpServerBinding::discover(
            "remote",
            Arc::new(MockClient::new(vec![tool("read_file")])),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let composite = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]).unwrap();
        assert!(composite.certifies_parallel_read("read_file"));
        assert!(!composite.certifies_parallel_read("mcp__remote__read_file"));
        assert!(!composite.certifies_parallel_read("unknown"));
    }

    #[tokio::test]
    async fn ambiguous_transport_failure_is_not_a_definitive_tool_error() {
        let mut failing = MockClient::new(vec![tool("effect")]);
        failing.uncertain = true;
        let mock = Arc::new(failing);
        let binding = McpServerBinding::discover("remote", mock.clone(), &CancellationToken::new())
            .await
            .unwrap();
        let composite = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]).unwrap();
        let mut invalid = call("mcp__remote__effect");
        invalid.arguments = json!({"value": 42});
        assert!(
            composite
                .execute(&invalid, &context())
                .await
                .unwrap()
                .is_error
        );
        assert!(mock.calls.lock().unwrap().is_empty());
        let error = composite
            .execute(&call("mcp__remote__effect"), &context())
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentError::EffectUncertain(message) if message.contains("connection lost"))
        );
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn maps_mcp_tool_error_to_tool_output_error() {
        let mut client = MockClient::new(vec![tool("fail")]);
        client.tool_error = true;
        let mock = Arc::new(client);
        let binding = McpServerBinding::discover("remote", mock, &CancellationToken::new())
            .await
            .unwrap();
        let composite = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]).unwrap();
        let output = composite
            .execute(&call("mcp__remote__fail"), &context())
            .await
            .unwrap();
        assert!(output.is_error);
        assert_eq!(output.content, "done");
    }

    #[test]
    fn preserves_structured_and_nontext_result_as_json() {
        let result = McpCallResult {
            content: vec![json!({"type":"resource_link","uri":"test://item","name":"item"})],
            is_error: true,
            structured_content: Some(json!({"value":1})),
            extra: BTreeMap::new(),
        };
        let text = format_mcp_result(&result);
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["structuredContent"], json!({"value":1}));
        assert_eq!(json["content"][0]["type"], "resource_link");
        assert_eq!(json["isError"], true);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn stdio_client_dispatches_through_composite_executor() {
        use crate::McpStdioConfig;
        use std::ffi::OsString;
        use std::time::Duration;
        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("composite-mcp-fixture.ps1");
        let script = r#"
while ($null -ne ($line = [Console]::In.ReadLine())) {
    $request = $line | ConvertFrom-Json
    if ($request.method -eq 'initialize') {
        $result = @{ protocolVersion = '2025-11-25'; capabilities = @{ tools = @{} }; serverInfo = @{ name = 'fixture'; version = '1' } }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    } elseif ($request.method -eq 'tools/list') {
        $result = @{ tools = @(@{ name = 'echo'; inputSchema = @{ type = 'object' } }) }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    } elseif ($request.method -eq 'tools/call') {
        $result = @{ content = @(@{ type = 'text'; text = $request.params.name }); isError = $false }
        [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Compress -Depth 20))
    }
}
"#;
        std::fs::write(&script_path, script).unwrap();
        let mut config = McpStdioConfig::new("powershell.exe");
        config.args = vec![
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-ExecutionPolicy"),
            OsString::from("Bypass"),
            OsString::from("-File"),
            script_path.into_os_string(),
        ];
        config.inherit_env = true;
        config.request_timeout = Duration::from_secs(10);
        let client = Arc::new(McpStdioClient::connect(config).await.unwrap());
        let cancellation = CancellationToken::new();
        let binding = McpServerBinding::discover("fixture", client.clone(), &cancellation)
            .await
            .unwrap();
        let composite = McpCompositeTools::new(Arc::new(LocalTools::new()), vec![binding]).unwrap();
        let output = composite
            .execute(
                &ToolCall {
                    id: "real-call".to_owned(),
                    name: "mcp__fixture__echo".to_owned(),
                    arguments: json!({}),
                },
                &context(),
            )
            .await
            .unwrap();
        assert_eq!(output.content, "echo");
        client.shutdown().await.unwrap();
    }
}
