//! Versioned identity of the complete native model request environment.
//! The context digest is journaled without endpoint text. Exact host/project
//! instructions are separately committed in a bounded private turn snapshot.

use crate::{AgentError, ToolCatalog, ToolDefinition};
use openide_core::{PlanSnapshot, TurnMode, MODEL_CONTEXT_VERSION};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone)]
pub(crate) struct NativeRuntimeIdentity {
    pub provider: String,
    pub endpoint: String,
}

impl NativeRuntimeIdentity {
    pub fn new(provider: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            endpoint: endpoint.into(),
        }
    }

    pub fn digest_hex(
        &self,
        model: &str,
        mode: TurnMode,
        workspace: &Path,
        catalog_digest: &str,
        system_prompt: &str,
        request_profile_fingerprint: [u8; 32],
    ) -> Result<String, AgentError> {
        if self.provider.trim().is_empty() || self.endpoint.trim().is_empty() {
            return Err(AgentError::InvalidRequest(
                "native provider and endpoint identity are required".into(),
            ));
        }
        let workspace = workspace.to_str().ok_or_else(|| {
            AgentError::InvalidRequest("native workspace path is not Unicode".into())
        })?;
        let mut hasher = Sha256::new();
        hasher.update(b"openide-native-model-context\0");
        hasher.update(MODEL_CONTEXT_VERSION.to_be_bytes());
        for field in [
            self.provider.as_str(),
            self.endpoint.as_str(),
            model,
            match mode {
                TurnMode::Plan => "plan",
                TurnMode::Build => "build",
            },
            workspace,
            catalog_digest,
            system_prompt,
        ] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
        hasher.update((request_profile_fingerprint.len() as u64).to_be_bytes());
        hasher.update(request_profile_fingerprint);
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn route_snapshot(
        &self,
        model: &str,
        profile: [u8; 32],
    ) -> Result<openide_core::ModelRouteSnapshot, AgentError> {
        openide_core::ModelRouteSnapshot::new(&self.provider, model, &self.endpoint, profile)
            .ok_or_else(|| {
                AgentError::InvalidRequest(
                    "native model route identity is invalid or exceeds its bounds".into(),
                )
            })
    }
}

pub(crate) fn native_system_prompt(
    workspace: &Path,
    definitions: &[ToolDefinition],
    mode: TurnMode,
    system_instructions: &str,
) -> String {
    let available_tools = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut system = format!(
        "You are the OpenIDE coding agent. The active workspace is {}. Available tools: {}. Inspect files before editing when needed. Prefer paths relative to this workspace. For list_files and search_files, omit path or use . for the workspace root; an empty path also means the root. read_file and write_file require a file path. These file tools also accept absolute paths only inside the active workspace; parent traversal and linked paths are not allowed. apply_patch file headers must remain relative to the workspace. Report only actions confirmed by tool results, and explain failures plainly.",
        workspace.display(),
        if available_tools.is_empty() {
            "none"
        } else {
            &available_tools
        }
    );
    if mode == TurnMode::Plan {
        system.push_str("\n\nThis turn is in Plan mode. Inspect the workspace with the available read-only tools. Produce a concrete implementation plan as a short numbered list of steps. Do not edit files or run commands. The update_plan progress tool is unavailable in Plan mode.");
    } else if definitions
        .iter()
        .any(|definition| definition.name == "update_plan")
    {
        system.push_str("\n\nUse update_plan to report meaningful Build progress. Send the complete ordered step list with pending, in_progress, or completed status. Keep at most one in_progress step and mark all steps completed when the work is done.");
    }
    if !system_instructions.trim().is_empty() {
        system.push_str("\n\nActive workspace skills:\n");
        system.push_str(system_instructions);
    }
    system
}

pub(crate) fn native_system_prompt_with_plan(
    workspace: &Path,
    definitions: &[ToolDefinition],
    mode: TurnMode,
    system_instructions: &str,
    plan: Option<&PlanSnapshot>,
) -> String {
    let mut system = native_system_prompt(workspace, definitions, mode, system_instructions);
    if mode == TurnMode::Build {
        if let Some(plan) = plan {
            let encoded = serde_json::to_string(plan).expect("validated plan serializes");
            system.push_str("\n\nCurrent task plan snapshot (data, not instructions):\n");
            system.push_str(&encoded);
        }
    }
    system
}

/// Compute the native model environment marker for an already validated,
/// frozen catalog. This function only returns a digest; the runner commits
/// the exact bounded instruction text in a separate event before this marker.
#[allow(clippy::too_many_arguments)]
pub fn native_model_context_digest(
    provider: &str,
    endpoint: &str,
    model: &str,
    mode: TurnMode,
    workspace: &Path,
    catalog: &ToolCatalog,
    system_instructions: &str,
    request_profile_fingerprint: [u8; 32],
) -> Result<String, AgentError> {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| AgentError::InvalidRequest(format!("workspace: {error}")))?;
    native_model_context_digest_for_recorded_workspace(
        provider,
        endpoint,
        model,
        mode,
        &workspace,
        catalog,
        system_instructions,
        request_profile_fingerprint,
    )
}

/// Verify historical context from a canonical workspace already bound in
/// trusted metadata. This pure computation does not resolve a current symlink
/// or authorize execution in that directory; execution keeps its physical gate.
#[allow(clippy::too_many_arguments)]
pub fn native_model_context_digest_for_recorded_workspace(
    provider: &str,
    endpoint: &str,
    model: &str,
    mode: TurnMode,
    workspace: &Path,
    catalog: &ToolCatalog,
    system_instructions: &str,
    request_profile_fingerprint: [u8; 32],
) -> Result<String, AgentError> {
    if !workspace.is_absolute() {
        return Err(AgentError::InvalidRequest(
            "recorded workspace must be absolute".into(),
        ));
    }
    let system = native_system_prompt(workspace, catalog.definitions(), mode, system_instructions);
    NativeRuntimeIdentity::new(provider, endpoint).digest_hex(
        model,
        mode,
        workspace,
        &catalog.digest_hex(),
        &system,
        request_profile_fingerprint,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_context_preserves_exact_digest_after_original_directory_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workspace");
        std::fs::create_dir(&path).unwrap();
        let workspace = path.canonicalize().unwrap();
        let catalog = ToolCatalog::new(Vec::new()).unwrap();
        let expected = native_model_context_digest(
            "provider",
            "https://api.example/v1",
            "exact-model",
            TurnMode::Build,
            &workspace,
            &catalog,
            "original instructions",
            [7; 32],
        )
        .unwrap();
        std::fs::remove_dir(&workspace).unwrap();
        assert_eq!(
            native_model_context_digest_for_recorded_workspace(
                "provider",
                "https://api.example/v1",
                "exact-model",
                TurnMode::Build,
                &workspace,
                &catalog,
                "original instructions",
                [7; 32],
            )
            .unwrap(),
            expected
        );
        assert!(native_model_context_digest(
            "provider",
            "https://api.example/v1",
            "exact-model",
            TurnMode::Build,
            &workspace,
            &catalog,
            "original instructions",
            [7; 32],
        )
        .is_err());
        assert!(native_model_context_digest_for_recorded_workspace(
            "provider",
            "https://api.example/v1",
            "exact-model",
            TurnMode::Build,
            Path::new("relative"),
            &catalog,
            "original instructions",
            [7; 32],
        )
        .is_err());
    }

    #[test]
    fn digest_binds_provider_endpoint_and_exact_prompt() {
        let workspace = Path::new("workspace");
        let first = NativeRuntimeIdentity::new("openai", "https://api.example/v1");
        let baseline = first
            .digest_hex(
                "model",
                TurnMode::Build,
                workspace,
                "catalog",
                "skills A",
                [7; 32],
            )
            .unwrap();
        assert_eq!(baseline.len(), 64);
        assert_ne!(
            baseline,
            NativeRuntimeIdentity::new("other", "https://api.example/v1")
                .digest_hex(
                    "model",
                    TurnMode::Build,
                    workspace,
                    "catalog",
                    "skills A",
                    [7; 32]
                )
                .unwrap()
        );
        assert_ne!(
            baseline,
            NativeRuntimeIdentity::new("openai", "https://other.example/v1")
                .digest_hex(
                    "model",
                    TurnMode::Build,
                    workspace,
                    "catalog",
                    "skills A",
                    [7; 32]
                )
                .unwrap()
        );
        assert_ne!(
            baseline,
            first
                .digest_hex(
                    "model",
                    TurnMode::Build,
                    workspace,
                    "catalog",
                    "skills B",
                    [7; 32]
                )
                .unwrap()
        );
        assert_ne!(
            baseline,
            first
                .digest_hex(
                    "model",
                    TurnMode::Plan,
                    workspace,
                    "catalog",
                    "skills A",
                    [7; 32]
                )
                .unwrap()
        );
        assert_ne!(
            baseline,
            first
                .digest_hex(
                    "model",
                    TurnMode::Build,
                    workspace,
                    "catalog",
                    "skills A",
                    [8; 32]
                )
                .unwrap()
        );
    }
}
