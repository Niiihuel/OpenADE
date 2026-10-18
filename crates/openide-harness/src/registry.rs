//! Immutable, validated tool definitions for one model request.
//!
//! A runner can create this snapshot from its executor, advertise exactly its
//! `definitions`, and check every returned call with `require_advertised`
//! before dispatch. This module intentionally does not execute tools.

use crate::{ToolDefinition, ToolExecutor};
use openide_core::ToolCall;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

pub const MAX_ADVERTISED_TOOLS: usize = 128;
pub const MAX_TOOL_NAME_BYTES: usize = 64;
pub const MAX_TOOL_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_CATALOG_SCHEMA_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 512 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ToolCatalogError {
    #[error("tool catalog exceeds {limit} definitions")]
    TooManyTools { limit: usize },
    #[error("invalid stable tool name: {name}")]
    InvalidName { name: String },
    #[error("duplicate tool name in advertised catalog: {name}")]
    DuplicateName { name: String },
    #[error("tool {name} has invalid object parameters: {reason}")]
    InvalidParameters { name: String, reason: String },
    #[error("schema for tool {name} exceeds {limit} bytes")]
    SchemaTooLarge { name: String, limit: usize },
    #[error("tool schemas together exceed {limit} bytes")]
    CatalogTooLarge { limit: usize },
    #[error("tool was not advertised for this model request: {name}")]
    UnadvertisedTool { name: String },
    #[error("invalid arguments for tool {name}: {reason}")]
    InvalidArguments { name: String, reason: String },
}

/// A frozen set of function definitions, indexed by exact tool name.
///
/// Clones share the same immutable definitions. The executor may change its
/// available tools after construction without changing this snapshot.
#[derive(Clone, Debug)]
pub struct ToolCatalog {
    definitions: Arc<[ToolDefinition]>,
    by_name: Arc<BTreeMap<String, usize>>,
    validators: Arc<[jsonschema::Validator]>,
}

impl ToolCatalog {
    /// Stable fingerprint of the exact ordered definitions advertised to a
    /// model. A changed description, schema, name, or order changes the key.
    /// The journal stores this before the first model request of a turn.
    pub fn digest_hex(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"openide-tool-catalog-v1\0");
        for definition in self.definitions() {
            let encoded = serde_json::to_vec(&serde_json::json!({
                "name": definition.name,
                "description": definition.description,
                "parameters": definition.parameters,
            }))
            .expect("tool definitions contain JSON values");
            hasher.update((encoded.len() as u64).to_be_bytes());
            hasher.update(encoded);
        }
        format!("{:x}", hasher.finalize())
    }

    pub fn new(
        definitions: impl IntoIterator<Item = ToolDefinition>,
    ) -> Result<Self, ToolCatalogError> {
        let mut collected = Vec::new();
        let mut by_name = BTreeMap::new();
        let mut validators = Vec::new();
        let mut total_schema_bytes = 0usize;
        for definition in definitions {
            if collected.len() >= MAX_ADVERTISED_TOOLS {
                return Err(ToolCatalogError::TooManyTools {
                    limit: MAX_ADVERTISED_TOOLS,
                });
            }
            validate_name(&definition.name)?;
            if by_name.contains_key(&definition.name) {
                return Err(ToolCatalogError::DuplicateName {
                    name: definition.name,
                });
            }
            validate_parameters(&definition)?;
            let size = serde_json::to_vec(&definition.parameters)
                .expect("serde_json::Value always serializes")
                .len();
            if size > MAX_TOOL_SCHEMA_BYTES {
                return Err(ToolCatalogError::SchemaTooLarge {
                    name: definition.name,
                    limit: MAX_TOOL_SCHEMA_BYTES,
                });
            }
            total_schema_bytes = total_schema_bytes.saturating_add(size);
            if total_schema_bytes > MAX_CATALOG_SCHEMA_BYTES {
                return Err(ToolCatalogError::CatalogTooLarge {
                    limit: MAX_CATALOG_SCHEMA_BYTES,
                });
            }
            let validator = compile_validator(&definition)?;
            by_name.insert(definition.name.clone(), collected.len());
            collected.push(definition);
            validators.push(validator);
        }
        Ok(Self {
            definitions: Arc::from(collected),
            by_name: Arc::new(by_name),
            validators: Arc::from(validators),
        })
    }

    pub fn from_executor(executor: &dyn ToolExecutor) -> Result<Self, ToolCatalogError> {
        Self::new(executor.definitions())
    }

    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.by_name
            .get(name)
            .and_then(|index| self.definitions.get(*index))
    }

    pub fn require_advertised(&self, call: &ToolCall) -> Result<&ToolDefinition, ToolCatalogError> {
        self.get(&call.name)
            .ok_or_else(|| ToolCatalogError::UnadvertisedTool {
                name: call.name.clone(),
            })
    }

    /// Check a returned call against the exact Draft 2020-12 schema compiled
    /// when the definition was advertised.
    pub fn require_valid_call(&self, call: &ToolCall) -> Result<&ToolDefinition, ToolCatalogError> {
        let definition = self.require_advertised(call)?;
        let error = |reason: String| ToolCatalogError::InvalidArguments {
            name: call.name.clone(),
            reason,
        };
        let size = serde_json::to_vec(&call.arguments)
            .expect("serde_json::Value always serializes")
            .len();
        if size > MAX_TOOL_ARGUMENT_BYTES {
            return Err(error(format!(
                "arguments exceed the {MAX_TOOL_ARGUMENT_BYTES} byte limit"
            )));
        }
        let index = self.by_name[&call.name];
        self.validators[index]
            .validate(&call.arguments)
            .map_err(|validation_error| error(validation_error.to_string()))?;
        Ok(definition)
    }
}

/// The Draft 2020-12 validator a definition's parameters compile to; the
/// catalog keeps one per advertised tool and the MCP adapter one per
/// deferred tool.
pub(crate) fn compile_validator(
    definition: &ToolDefinition,
) -> Result<jsonschema::Validator, ToolCatalogError> {
    jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .offline()
        .build(&definition.parameters)
        .map_err(|error| ToolCatalogError::InvalidParameters {
            name: definition.name.clone(),
            reason: error.to_string(),
        })
}

fn validate_name(name: &str) -> Result<(), ToolCatalogError> {
    let mut bytes = name.bytes();
    let valid = name.len() <= MAX_TOOL_NAME_BYTES
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(ToolCatalogError::InvalidName {
            name: name.to_owned(),
        })
    }
}

fn validate_parameters(definition: &ToolDefinition) -> Result<(), ToolCatalogError> {
    let invalid = |reason: &'static str| ToolCatalogError::InvalidParameters {
        name: definition.name.clone(),
        reason: reason.to_owned(),
    };
    let Some(schema) = definition.parameters.as_object() else {
        return Err(invalid("schema root must be a JSON object"));
    };
    if schema.get("type").and_then(|value| value.as_str()) != Some("object") {
        return Err(invalid("schema root must declare type object"));
    }
    if schema
        .get("properties")
        .is_some_and(|value| !value.is_object())
    {
        return Err(invalid("properties must be an object"));
    }
    if schema.get("required").is_some_and(|value| {
        !value
            .as_array()
            .is_some_and(|items| items.iter().all(|item| item.as_str().is_some()))
    }) {
        return Err(invalid("required must be an array of property names"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalTool, LocalTools};
    use serde_json::json;

    fn definition(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            description: "Test tool".into(),
            parameters: json!({
                "type":"object",
                "properties":{"path":{"type":"string"}},
                "required":["path"],
                "additionalProperties":false
            }),
        }
    }

    #[test]
    fn snapshots_an_executor_and_rejects_unadvertised_calls() {
        let executor = LocalTools::new();
        let catalog = ToolCatalog::from_executor(&executor).unwrap();
        assert_eq!(catalog.definitions().len(), 3);
        assert!(catalog.get("read_file").is_some());
        let write_call = ToolCall {
            id: "call-1".into(),
            name: "write_file".into(),
            arguments: json!({"path":"a","content":"b"}),
        };
        assert_eq!(
            catalog.require_advertised(&write_call),
            Err(ToolCatalogError::UnadvertisedTool {
                name: "write_file".into()
            })
        );
        let writable = LocalTools::new().with_tool_permission(LocalTool::WriteFile, true);
        let newer_catalog = ToolCatalog::from_executor(&writable).unwrap();
        assert!(newer_catalog.require_advertised(&write_call).is_ok());
        assert!(catalog.get("write_file").is_none());
    }

    #[test]
    fn digest_covers_order_name_description_and_schema() {
        let first = definition("read_file");
        let second = definition("search_files");
        let baseline = ToolCatalog::new([first.clone(), second.clone()])
            .unwrap()
            .digest_hex();
        assert_eq!(baseline.len(), 64);
        assert_eq!(
            baseline,
            ToolCatalog::new([first.clone(), second.clone()])
                .unwrap()
                .digest_hex()
        );
        assert_ne!(
            baseline,
            ToolCatalog::new([second.clone(), first.clone()])
                .unwrap()
                .digest_hex()
        );
        let mut changed_description = first.clone();
        changed_description.description.push_str(" more");
        assert_ne!(
            baseline,
            ToolCatalog::new([changed_description, second.clone()])
                .unwrap()
                .digest_hex()
        );
        let mut changed_schema = first;
        changed_schema.parameters["properties"]["path"]["maxLength"] = json!(12);
        assert_ne!(
            baseline,
            ToolCatalog::new([changed_schema, second])
                .unwrap()
                .digest_hex()
        );
    }

    #[test]
    fn preserves_original_definition_after_input_changes() {
        let mut definitions = vec![definition("mcp__server__read")];
        let catalog = ToolCatalog::new(definitions.clone()).unwrap();
        definitions[0].description = "changed".into();
        definitions[0].parameters["properties"]["path"]["type"] = json!("integer");
        let shared = catalog.clone();
        assert_eq!(
            shared.get("mcp__server__read").unwrap().description,
            "Test tool"
        );
        assert_eq!(
            catalog.get("mcp__server__read").unwrap().parameters["properties"]["path"]["type"],
            "string"
        );
    }

    #[test]
    fn rejects_duplicate_and_unstable_names() {
        assert_eq!(
            ToolCatalog::new(vec![definition("read_file"), definition("read_file")]).unwrap_err(),
            ToolCatalogError::DuplicateName {
                name: "read_file".into()
            }
        );
        for name in ["", "9first", "has space", "has/slash", "café"] {
            assert!(matches!(
                ToolCatalog::new(vec![definition(name)]),
                Err(ToolCatalogError::InvalidName { .. })
            ));
        }
        assert!(matches!(
            ToolCatalog::new(vec![definition(&"a".repeat(MAX_TOOL_NAME_BYTES + 1))]),
            Err(ToolCatalogError::InvalidName { .. })
        ));
    }

    #[test]
    fn validates_object_schema_and_bounds() {
        for parameters in [
            json!([]),
            json!({"type":"string"}),
            json!({"type":"object","properties":[]}),
            json!({"type":"object","required":[1]}),
        ] {
            let mut tool = definition("bad");
            tool.parameters = parameters;
            assert!(matches!(
                ToolCatalog::new(vec![tool]),
                Err(ToolCatalogError::InvalidParameters { .. })
            ));
        }
        let mut huge = definition("huge");
        huge.parameters["description"] = json!("x".repeat(MAX_TOOL_SCHEMA_BYTES));
        assert!(matches!(
            ToolCatalog::new(vec![huge]),
            Err(ToolCatalogError::SchemaTooLarge { .. })
        ));
        let too_many = (0..=MAX_ADVERTISED_TOOLS)
            .map(|index| definition(&format!("tool_{index}")))
            .collect::<Vec<_>>();
        assert_eq!(
            ToolCatalog::new(too_many).unwrap_err(),
            ToolCatalogError::TooManyTools {
                limit: MAX_ADVERTISED_TOOLS
            }
        );
    }

    #[test]
    fn limits_total_schema_bytes() {
        let mut definitions = Vec::new();
        for index in 0..40 {
            let mut tool = definition(&format!("tool_{index}"));
            tool.parameters["description"] = json!("x".repeat(60 * 1024));
            definitions.push(tool);
        }
        assert_eq!(
            ToolCatalog::new(definitions).unwrap_err(),
            ToolCatalogError::CatalogTooLarge {
                limit: MAX_CATALOG_SCHEMA_BYTES
            }
        );
    }

    #[test]
    fn validates_local_arguments_before_dispatch() {
        let catalog = ToolCatalog::from_executor(&LocalTools::new()).unwrap();
        let call = |arguments| ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments,
        };
        assert!(catalog
            .require_valid_call(&call(json!({"path":"src/main.rs"})))
            .is_ok());
        for arguments in [
            json!([]),
            json!({}),
            json!({"path":42}),
            json!({"path":"src/main.rs","unknown":true}),
        ] {
            assert!(matches!(
                catalog.require_valid_call(&call(arguments)),
                Err(ToolCatalogError::InvalidArguments { .. })
            ));
        }
    }

    #[test]
    fn validates_nested_arrays_bounds_and_combinators() {
        let mut tool = definition("nested");
        tool.parameters = json!({
            "type":"object",
            "properties": {
                "values": {"type":"array", "items":{"type":"integer", "minimum":1}, "minItems":1},
                "choice": {"oneOf":[{"const":"a"},{"const":"b"}]}
            },
            "required":["values","choice"],
            "additionalProperties":false
        });
        let catalog = ToolCatalog::new([tool]).unwrap();
        let call = |arguments| ToolCall {
            id: "call-1".into(),
            name: "nested".into(),
            arguments,
        };
        assert!(catalog
            .require_valid_call(&call(json!({"values":[1,2],"choice":"b"})))
            .is_ok());
        for arguments in [
            json!({"values":[],"choice":"a"}),
            json!({"values":[0],"choice":"a"}),
            json!({"values":["1"],"choice":"a"}),
            json!({"values":[1],"choice":"c"}),
        ] {
            assert!(matches!(
                catalog.require_valid_call(&call(arguments)),
                Err(ToolCatalogError::InvalidArguments { .. })
            ));
        }
    }

    #[test]
    fn resolves_local_refs_and_enforces_mcp_object_constraints() {
        let mut tool = definition("referenced");
        tool.parameters = json!({
            "type":"object",
            "$defs": {"path": {"type":"string", "minLength":3, "pattern":"^[a-z]+$"}},
            "properties": {
                "path": {"$ref":"#/$defs/path"},
                "metadata": {"type":"object", "additionalProperties":{"type":"integer", "minimum":0}}
            },
            "required":["path"],
            "additionalProperties":false
        });
        let catalog = ToolCatalog::new([tool]).unwrap();
        let call = |arguments| ToolCall {
            id: "call-1".into(),
            name: "referenced".into(),
            arguments,
        };
        assert!(catalog
            .require_valid_call(&call(json!({"path":"alpha","metadata":{"n":2}})))
            .is_ok());
        for arguments in [
            json!({"path":"a"}),
            json!({"path":"Alpha"}),
            json!({"path":"alpha","metadata":{"n":-1}}),
            json!({"path":"alpha","extra":true}),
        ] {
            assert!(matches!(
                catalog.require_valid_call(&call(arguments)),
                Err(ToolCatalogError::InvalidArguments { .. })
            ));
        }
    }

    #[test]
    fn one_of_requires_exactly_one_match() {
        let mut tool = definition("choice");
        tool.parameters = json!({
            "type":"object",
            "properties": {
                "value": {"oneOf": [
                    {"type":"integer", "minimum":0},
                    {"type":"number", "maximum":10}
                ]}
            },
            "required":["value"],
            "additionalProperties":false
        });
        let catalog = ToolCatalog::new([tool]).unwrap();
        let call = |value| ToolCall {
            id: "call-1".into(),
            name: "choice".into(),
            arguments: json!({"value":value}),
        };
        assert!(catalog.require_valid_call(&call(json!(11))).is_ok());
        assert!(catalog.require_valid_call(&call(json!(-1))).is_ok());
        assert!(catalog.require_valid_call(&call(json!(5))).is_err());
        assert!(catalog.require_valid_call(&call(json!("five"))).is_err());
    }

    #[test]
    fn validates_formats_and_rejects_invalid_schemas_before_dispatch() {
        let mut tool = definition("formatted");
        tool.parameters["properties"]["path"] = json!({"anyOf":[
            {"type":"string", "format":"email"},
            {"type":"string", "format":"uuid"}
        ]});
        let catalog = ToolCatalog::new([tool]).unwrap();
        let call = |path| ToolCall {
            id: "call-1".into(),
            name: "formatted".into(),
            arguments: json!({"path":path}),
        };
        assert!(catalog
            .require_valid_call(&call("user@example.com"))
            .is_ok());
        assert!(catalog
            .require_valid_call(&call("550e8400-e29b-41d4-a716-446655440000"))
            .is_ok());
        assert!(catalog.require_valid_call(&call("not-an-email")).is_err());

        for invalid_path_schema in [
            json!({"$ref":"#/$defs/missing"}),
            json!({"$ref":"https://example.invalid/schema.json"}),
            json!({"type":"not-a-json-type"}),
            json!({"type":"string", "pattern":"["}),
            json!({"type":"string", "format":"unknown-format"}),
        ] {
            let mut invalid_tool = definition("invalid");
            invalid_tool.parameters["properties"]["path"] = invalid_path_schema;
            assert!(matches!(
                ToolCatalog::new([invalid_tool]),
                Err(ToolCatalogError::InvalidParameters { .. })
            ));
        }
    }

    #[test]
    fn rejects_oversized_arguments() {
        let catalog = ToolCatalog::new([definition("large")]).unwrap();
        let call = ToolCall {
            id: "call-2".into(),
            name: "large".into(),
            arguments: json!({"path":"x".repeat(MAX_TOOL_ARGUMENT_BYTES)}),
        };
        assert!(matches!(
            catalog.require_valid_call(&call),
            Err(ToolCatalogError::InvalidArguments { .. })
        ));
    }
}
