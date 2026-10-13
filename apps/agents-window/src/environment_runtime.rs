//! Prepare conversation resources on the runner runtime, after its input
//! validation and before executing tools. Presentation never grants effects.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use openide_core::AgentEvent;
use openide_harness::{
    AgentError, McpCompositeTools, McpServerBinding, McpToolClient, PreparedToolSession,
    ToolExecutor, ToolPreparation,
};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    browser_runtime::ConversationBrowser,
    image_runtime::ImageMcpClient,
    mcp_runtime::McpToolPreparation,
    mcp_settings::{McpServerSettings, McpSettings},
};

pub(crate) struct EnvironmentToolPreparation {
    storage_root: PathBuf,
    session_id: String,
    prior_events: Arc<Vec<AgentEvent>>,
    servers: Vec<McpServerSettings>,
    browser: Option<ConversationBrowser>,
    browser_allowed: bool,
    image: Option<ImageMcpClient>,
    plugin_root: Option<PathBuf>,
}

impl EnvironmentToolPreparation {
    pub fn new(
        storage_root: PathBuf,
        session_id: String,
        prior_events: Arc<Vec<AgentEvent>>,
        servers: Vec<McpServerSettings>,
        browser: Option<ConversationBrowser>,
    ) -> Self {
        Self {
            storage_root,
            session_id,
            prior_events,
            servers,
            browser,
            browser_allowed: false,
            image: None,
            plugin_root: None,
        }
    }

    /// The user allowed everything in this conversation: the browser this
    /// window serves does not ask either. The host's policy has to allow its
    /// tools too, or they stay denied as any tool it does not name.
    pub fn with_browser_allowed(mut self, allowed: bool) -> Self {
        self.browser_allowed = allowed;
        self
    }

    /// Image generation writes a file and spends money: the caller offers it
    /// only in a turn that may write, and every call still asks.
    pub fn with_image_generation(mut self, image: Option<ImageMcpClient>) -> Self {
        self.image = image;
        self
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
        // The tools this window serves itself. Each of them asks every time,
        // but the browser's that only read the page, and all of the
        // browser's once the user has allowed everything.
        let mut own: Vec<(&str, Arc<dyn McpToolClient>)> = Vec::new();
        if let Some(browser) = &self.browser {
            own.push(("openide_browser", Arc::new(browser.clone())));
        }
        if let Some(image) = &self.image {
            own.push((crate::image_runtime::SERVER, Arc::new(image.clone())));
        }
        if !own.is_empty() {
            let composite = async {
                let mut bindings = Vec::new();
                for (name, client) in own {
                    bindings.push(
                        McpServerBinding::discover(name, client, cancellation)
                            .await
                            .map_err(|error| AgentError::Runtime(error.to_string()))?,
                    );
                }
                McpCompositeTools::new(tools.clone(), bindings)
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
            let browser_allowed = self.browser_allowed;
            asks.extend(
                composite
                    .mcp_tool_names()
                    .filter(|name| {
                        !crate::browser_runtime::native_tool_allowed(name, browser_allowed)
                    })
                    .map(str::to_owned),
            );
            tools = Arc::new(composite);
        }
        let external_identity = inner
            .as_ref()
            .and_then(|prepared| prepared.mcp_resume_identity_digest());
        // The browser's identity is the same in every turn: its tab opens on
        // a call, so no live browser is bound to the turn.
        // An unverified external MCP identity remains unverified after composition.
        let serves_own = self.browser.is_some() || self.image.is_some();
        let resume_identity = match (serves_own, inner.as_ref(), external_identity) {
            (false, _, identity) => identity.map(str::to_owned),
            (true, Some(_), None) => None,
            (true, _, identity) => {
                let mut hash = Sha256::new();
                hash.update(b"openide-conversation-environment-v1\0");
                hash.update(identity.unwrap_or("no-external-mcp").as_bytes());
                hash.update([0]);
                if let Some(browser) = &self.browser {
                    hash.update(browser.identity_digest().as_bytes());
                }
                // After a separator the browser's digest never holds, so a
                // turn with only the browser keeps the identity it had.
                if let Some(image) = &self.image {
                    hash.update([0]);
                    hash.update(image.identity_digest().as_bytes());
                }
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
    async fn image_generation_is_offered_only_when_bound_and_asks_even_under_full_access() {
        use openide_harness::{LocalTools, PermissionMode, PermissionPolicy};
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let (root, workspace) = (root.path(), workspace.path());
        let prepare = |image| async move {
            EnvironmentToolPreparation::new(
                root.to_path_buf(),
                "fixture".into(),
                Arc::new(Vec::new()),
                Vec::new(),
                None,
            )
            .with_image_generation(image)
            .prepare(workspace, Arc::new(LocalTools::new()), &CancellationToken::new())
            .await
            .unwrap()
        };
        let name = crate::image_runtime::TOOL;
        let offered = |prepared: &dyn PreparedToolSession| {
            prepared.tools().definitions().iter().any(|tool| tool.name == name)
        };
        // No connected provider, a Plan turn or a read-only one: nothing is bound.
        let without = prepare(None).await;
        assert!(!offered(without.as_ref()) && without.ask_tool_names().is_empty());
        assert!(without.mcp_resume_identity_digest().is_none());

        let client = ImageMcpClient::new(
            workspace,
            root,
            "http://127.0.0.1:9/v1",
            true,
            || Err("the credential is not read to offer the tool".into()),
        );
        let identity = client.identity_digest();
        let prepared = prepare(Some(client)).await;
        assert!(offered(prepared.as_ref()));
        assert_eq!(prepared.ask_tool_names(), [name.to_owned()]);
        // A verified identity of its own, so a session approval can be saved.
        let digest = prepared.mcp_resume_identity_digest().unwrap();
        assert!(digest.len() == 64 && digest != identity);

        let call = openide_core::ToolCall {
            id: "call".into(),
            name: name.into(),
            arguments: json!({"prompt":"Un faro","path":"faro.png"}),
        };
        let mode = |mut policy: PermissionPolicy| {
            for name in prepared.ask_tool_names() {
                policy.require_ask_unless_denied(name);
            }
            policy.mode_for_call_with_session_grants("fixture", workspace, workspace, "", &call, &[])
        };
        let open = PermissionPolicy::new().with_default_mode(PermissionMode::Allow);
        assert_eq!(mode(open.clone()), PermissionMode::Ask);
        assert_eq!(
            mode(open.with_tool(name, PermissionMode::Deny)),
            PermissionMode::Deny
        );
        // The window's own default denies what it does not know: still asked.
        assert_eq!(
            mode(PermissionPolicy::new().with_default_mode(PermissionMode::Deny)),
            PermissionMode::Ask
        );
    }

    #[tokio::test]
    async fn every_browser_tool_is_advertised_with_no_browser_open_and_all_but_reads_ask() {
        use openide_harness::{LocalTools, PermissionMode, PermissionPolicy};
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        // No tab is open: the turn is given the tools all the same.
        let closed = || ConversationBrowser::new(|| Err("sin navegador".into()));
        let prepared = EnvironmentToolPreparation::new(
            root.path().to_path_buf(),
            "fixture".into(),
            Arc::new(Vec::new()),
            Vec::new(),
            Some(closed()),
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
        // The catalog every client gets, and the native agent's `open_preview`.
        let expected: Vec<_> = expected
            .into_iter()
            .chain(["mcp__openide_browser__open_preview".to_owned()])
            .collect();
        assert_eq!(advertised.len(), 21);
        for name in &expected {
            assert!(advertised.contains(name), "{name} is not advertised");
            // Reading the page the user has open is the one thing not asked.
            assert_eq!(
                prepared.ask_tool_names().contains(name),
                !crate::browser_runtime::native_tool_allowed(name, false),
                "{name}"
            );
        }
        assert_eq!(prepared.ask_tool_names().len(), 16);
        // The same in every turn, so an approval for the session outlives the tab.
        assert!(prepared.mcp_resume_identity_digest().is_some());
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

        // Once the user allowed everything in the conversation, none of the
        // browser's tools is marked to ask, and the host's policy decides.
        let allowed = EnvironmentToolPreparation::new(
            root.path().to_path_buf(),
            "fixture".into(),
            Arc::new(Vec::new()),
            Vec::new(),
            Some(closed()),
        )
        .with_browser_allowed(true)
        .prepare(
            workspace.path(),
            Arc::new(LocalTools::new()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(allowed.ask_tool_names().is_empty(), "{:?}", allowed.ask_tool_names());
        assert_eq!(crate::browser_runtime::native_tool_names(), expected);
    }
}
