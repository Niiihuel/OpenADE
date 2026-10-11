//! Prepare conversation resources on the runner runtime, after its input
//! validation and before executing tools. Presentation never grants effects.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use openide_core::AgentEvent;
use openide_harness::{
    AgentError, McpCompositeTools, McpServerBinding, PreparedToolSession, ToolExecutor,
    ToolPreparation,
};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    browser_runtime::BrowserMcpClient,
    mcp_runtime::McpToolPreparation,
    mcp_settings::{McpServerSettings, McpSettings},
};

pub(crate) struct EnvironmentToolPreparation {
    storage_root: PathBuf,
    session_id: String,
    prior_events: Arc<Vec<AgentEvent>>,
    servers: Vec<McpServerSettings>,
    browser: Option<BrowserMcpClient>,
    plugin_root: Option<PathBuf>,
}

impl EnvironmentToolPreparation {
    pub fn new(
        storage_root: PathBuf,
        session_id: String,
        prior_events: Arc<Vec<AgentEvent>>,
        servers: Vec<McpServerSettings>,
        browser: Option<BrowserMcpClient>,
    ) -> Self {
        Self {
            storage_root,
            session_id,
            prior_events,
            servers,
            browser,
            plugin_root: None,
        }
    }

    /// Opt in only for a principal Build turn or its resume. Construction is
    /// passive; package verification takes place on the preparation worker.
    pub fn with_plugins(mut self, root: PathBuf) -> Self {
        self.plugin_root = Some(root);
        self
    }

    async fn runtime_servers(
        &self,
        workspace: &Path,
    ) -> Result<Vec<McpServerSettings>, AgentError> {
        let mut settings = McpSettings {
            servers: self.servers.clone(),
        };
        if let Some(root) = &self.plugin_root {
            let root = root.clone();
            let workspace = workspace.to_path_buf();
            settings = tokio::task::spawn_blocking(move || {
                settings
                    .servers
                    .extend(crate::plugins::PluginStore::runtime_servers(
                        &root, &workspace,
                    )?);
                // Include disabled manually configured entries when checking
                // limits and names. Do not reread the frozen manual snapshot.
                settings.validate_structure()?;
                Ok::<_, String>(settings)
            })
            .await
            .map_err(|error| AgentError::Runtime(format!("Preparación de plugins: {error}")))?
            .map_err(|error| AgentError::Runtime(format!("Preparación de plugins: {error}")))?;
        } else {
            settings.validate_structure().map_err(AgentError::Runtime)?;
        }
        Ok(settings
            .servers
            .into_iter()
            .filter(|server| server.enabled)
            .collect())
    }
}

#[async_trait]
impl ToolPreparation for EnvironmentToolPreparation {
    async fn prepare(
        &self,
        workspace: &Path,
        base_tools: Arc<dyn ToolExecutor>,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn PreparedToolSession>, AgentError> {
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let servers = self.runtime_servers(workspace).await?;
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let root = self.storage_root.clone();
        let session_id = self.session_id.clone();
        let prior = self.prior_events.clone();
        let workspace = workspace.to_path_buf();
        // A failed/unsupported baseline leaves the changes view unavailable;
        // it is never substituted with the current Git working tree.
        let snapshot_workspace = workspace.clone();
        let _ = tokio::task::spawn_blocking(move || {
            crate::session_changes::capture_baseline(
                &root,
                &session_id,
                &snapshot_workspace,
                &prior,
            )
        })
        .await;
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if !servers.is_empty() {
            let root = self.storage_root.clone();
            let session_id = self.session_id.clone();
            let workspace = workspace.clone();
            let after_sequence = self.prior_events.last().map_or(0, |event| event.sequence);
            tokio::task::spawn_blocking(move || {
                crate::session_changes_external::mark_external_runtime(
                    &root,
                    &session_id,
                    &workspace,
                    after_sequence,
                )
            })
            .await
            .map_err(|error| AgentError::Runtime(format!("Registro del entorno MCP: {error}")))?
            .map_err(|error| AgentError::Runtime(format!("Registro del entorno MCP: {error}")))?;
            if cancellation.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
        }
        let base_tools = crate::session_changes::with_file_snapshots(
            base_tools,
            self.storage_root.clone(),
            self.session_id.clone(),
            workspace.clone(),
        );
        let inner = if servers.is_empty() {
            None
        } else {
            Some(
                McpToolPreparation::new(servers)
                    .prepare(&workspace, base_tools.clone(), cancellation)
                    .await?,
            )
        };
        let mut tools = inner
            .as_ref()
            .map_or(base_tools, |prepared| prepared.tools());
        let mut asks = inner
            .as_ref()
            .map_or_else(Vec::new, |prepared| prepared.ask_tool_names().to_vec());
        if let Some(browser) = &self.browser {
            let composite = async {
                let binding = McpServerBinding::discover(
                    "openide_browser",
                    Arc::new(browser.clone()),
                    cancellation,
                )
                .await
                .map_err(|error| AgentError::Runtime(error.to_string()))?;
                McpCompositeTools::new(tools.clone(), vec![binding])
                    .map_err(|error| AgentError::Runtime(error.to_string()))
            }
            .await;
            let composite = match composite {
                Ok(composite) => composite,
                Err(error) => {
                    if let Some(prepared) = &inner {
                        let _ = prepared.shutdown().await;
                    }
                    return Err(if cancellation.is_cancelled() {
                        AgentError::Cancelled
                    } else {
                        error
                    });
                }
            };
            asks.extend(
                composite
                    .definitions()
                    .into_iter()
                    .filter(|tool| tool.name.starts_with("mcp__openide_browser__"))
                    .map(|tool| tool.name),
            );
            tools = Arc::new(composite);
        }
        let external_identity = inner
            .as_ref()
            .and_then(|prepared| prepared.mcp_resume_identity_digest());
        // The browser identity includes this live actor instance and its frozen
        // scope. Replacing a closed browser cannot resume an old tool batch.
        // An unverified external MCP identity remains unverified after composition.
        let resume_identity = match (&self.browser, inner.as_ref(), external_identity) {
            (None, _, identity) => identity.map(str::to_owned),
            (Some(_), Some(_), None) => None,
            (Some(browser), _, identity) => {
                let mut hash = Sha256::new();
                hash.update(b"openide-conversation-environment-v1\0");
                hash.update(identity.unwrap_or("no-external-mcp").as_bytes());
                hash.update([0]);
                hash.update(browser.identity_digest().as_bytes());
                Some(format!("{:x}", hash.finalize()))
            }
        };
        Ok(Box::new(EnvironmentPrepared {
            inner,
            tools,
            asks,
            resume_identity,
        }))
    }
}

struct EnvironmentPrepared {
    inner: Option<Box<dyn PreparedToolSession>>,
    tools: Arc<dyn ToolExecutor>,
    asks: Vec<String>,
    resume_identity: Option<String>,
}

#[async_trait]
impl PreparedToolSession for EnvironmentPrepared {
    fn tools(&self) -> Arc<dyn ToolExecutor> {
        self.tools.clone()
    }
    fn ask_tool_names(&self) -> &[String] {
        &self.asks
    }
    fn mcp_resume_identity_digest(&self) -> Option<&str> {
        self.resume_identity.as_deref()
    }
    async fn shutdown(&self) -> Result<(), AgentError> {
        match &self.inner {
            Some(prepared) => prepared.shutdown().await,
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::PluginStore;
    use serde_json::json;
    use std::fs;

    fn manual(id: &str) -> McpServerSettings {
        McpServerSettings {
            id: id.into(),
            enabled: false,
            program: "sh".into(),
            args: Vec::new(),
            cwd: None,
            credential_env_names: Vec::new(),
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
        }
    }

    #[tokio::test]
    async fn plugin_worker_preserves_manual_snapshot_and_validates_before_startup() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join("openide-plugin.json"),
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "id": "fixture",
                "name": "Fixture",
                "version": "1.0.0",
                "description": "Plugin worker validation fixture",
                "publisher": "OpenIDE tests",
                "program": "sh",
                "entrypoint": "server.sh"
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(source.path().join("server.sh"), "exit 99\n").unwrap();
        PluginStore::install(
            root.path(),
            workspace.path(),
            &PluginStore::inspect(source.path()).unwrap(),
        )
        .unwrap();
        PluginStore::set_enabled(root.path(), workspace.path(), "fixture", true).unwrap();
        let preparation = |servers| {
            EnvironmentToolPreparation::new(
                root.path().to_path_buf(),
                "fixture".into(),
                Arc::new(Vec::new()),
                servers,
                None,
            )
            .with_plugins(root.path().to_path_buf())
        };

        // A later edit of the MCP file must not replace the turn's manual
        // snapshot. The worker loads only installed-plugin activation there.
        let frozen = preparation(Vec::new());
        McpSettings {
            servers: vec![manual("plugin_fixture")],
        }
        .save(root.path(), workspace.path())
        .unwrap();
        let servers = frozen.runtime_servers(workspace.path()).await.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "plugin_fixture");

        // Disabled entries still reserve their namespace and count toward
        // the combined catalog's limit; nothing has been discovered yet.
        let error = preparation(vec![manual("plugin_fixture")])
            .runtime_servers(workspace.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("duplicado"), "{error}");
        let error = preparation((0..16).map(|i| manual(&format!("manual_{i}"))).collect())
            .runtime_servers(workspace.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("servidores MCP"), "{error}");

        // Verify bundle integrity at worker time, after a passive constructor.
        let installed = PluginStore::list(root.path(), workspace.path()).unwrap();
        let stale = preparation(Vec::new());
        fs::write(installed[0].path.join("server.sh"), "exit 0\n").unwrap();
        assert!(stale.runtime_servers(workspace.path()).await.is_err());
    }

    #[tokio::test]
    async fn every_browser_tool_is_advertised_and_requires_the_users_approval() {
        use openide_harness::{LocalTools, PermissionMode, PermissionPolicy};
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let (runtime, _commands) = crate::browser_runtime::tests::fixture(
            crate::browser_runtime::BrowserScope {
                workspace: workspace.path().to_path_buf(),
                session_id: "fixture".into(),
            },
            Default::default(),
        );
        let prepared = EnvironmentToolPreparation::new(
            root.path().to_path_buf(),
            "fixture".into(),
            Arc::new(Vec::new()),
            Vec::new(),
            Some(runtime.mcp_client()),
        )
        .prepare(
            workspace.path(),
            Arc::new(LocalTools::new()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let advertised: Vec<_> = prepared
            .tools()
            .definitions()
            .into_iter()
            .map(|tool| tool.name)
            .filter(|name| name.starts_with("mcp__openide_browser__"))
            .collect();
        let expected: Vec<_> = crate::browser_runtime::browser_mcp_tools()
            .into_iter()
            .map(|tool| format!("mcp__openide_browser__{}", tool.name))
            .collect();
        assert_eq!(advertised.len(), 20);
        for name in &expected {
            assert!(advertised.contains(name), "{name} is not advertised");
            assert!(prepared.ask_tool_names().contains(name), "{name} skips approval");
        }
        // What the runner does with that list: a host that allows everything
        // still asks before a page script, and a host denial is never relaxed.
        let evaluate = openide_core::ToolCall {
            id: "call".into(),
            name: "mcp__openide_browser__browser_evaluate".into(),
            arguments: json!({"expression":"document.cookie"}),
        };
        let mode = |mut policy: PermissionPolicy| {
            for name in prepared.ask_tool_names() {
                policy.require_ask_unless_denied(name);
            }
            policy.mode_for_call_with_session_grants(
                "fixture",
                workspace.path(),
                workspace.path(),
                "",
                &evaluate,
                &[],
            )
        };
        let open = PermissionPolicy::new().with_default_mode(PermissionMode::Allow);
        assert_eq!(mode(open.clone()), PermissionMode::Ask);
        assert_eq!(
            mode(open.with_tool(evaluate.name.clone(), PermissionMode::Deny)),
            PermissionMode::Deny
        );
    }
}
