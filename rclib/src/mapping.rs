use std::collections::HashMap;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlatSpec {
    pub commands: Vec<CommandSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    /// Optional name for hierarchical mapping. Required when used inside groups.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional about/description for help.
    #[serde(default)]
    pub about: Option<String>,
    /// Pattern like: "sysinfo get {id}" (optional for hierarchical commands)
    #[serde(default)]
    pub pattern: String,
    /// HTTP method like GET/POST (optional for scenario commands)
    #[serde(default)]
    pub method: Option<String>,
    /// Endpoint template like "/sysinfo/?id={id}" (optional for scenario commands)
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Optional body template
    #[serde(default)]
    pub body: Option<String>,
    /// Optional headers with template values
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Optional hint for rendering list responses as a table with specific columns
    #[serde(default)]
    pub table_view: Option<Vec<String>>,
    /// Optional scenario for multi-step operations
    #[serde(default)]
    pub scenario: Option<Scenario>,
    /// Whether this command uses multipart/form-data uploads
    #[serde(default)]
    pub multipart: bool,
    /// Optional custom handler name for imperative logic
    #[serde(default)]
    pub custom_handler: Option<String>,

    /// Optional argument specifications to aid CLI generation
    #[serde(default)]
    pub args: Vec<ArgSpec>,
    /// Optional list of common arg names to inherit from the parent group
    #[serde(default)]
    pub use_common_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArgSpec {
    /// Name of the variable used in endpoint/body/headers templates
    #[serde(default)]
    pub name: Option<String>,
    /// Inherit from parent's common_args[inherit]; merges & allows overrides
    #[serde(default)]
    pub inherit: Option<String>,
    /// Help text shown in CLI
    #[serde(default)]
    pub help: Option<String>,
    /// If true, expose as positional argument instead of --flag
    #[serde(default)]
    pub positional: Option<bool>,
    /// Long flag name (defaults to placeholder/name)
    #[serde(default)]
    pub long: Option<String>,
    /// Short flag (single character)
    #[serde(default)]
    pub short: Option<String>,
    /// Whether argument is required
    #[serde(default)]
    pub required: Option<bool>,
    /// Default value
    #[serde(default)]
    pub default: Option<String>,
    /// Argument type (e.g., "bool")
    #[serde(default, rename = "type")]
    pub arg_type: Option<String>,
    /// Conditional values for boolean flags
    #[serde(default)]
    pub value: Option<ConditionalValue>,
    /// Whether this argument represents a file to upload
    #[serde(default)]
    pub file_upload: bool,
    /// For type="file", specifies which variable this file should override
    #[serde(default, rename = "file-overrides-value-of")]
    pub file_overrides_value_of: Option<String>,

    // Optional per-arg overrides of the overall command
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConditionalValue {
    Mapping {
        if_set: Option<String>,
        if_not_set: Option<String>,
    },
    Sequence(Vec<ConditionalValueEntry>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalValueEntry {
    pub if_set: Option<String>,
    pub if_not_set: Option<String>,
}

// =====================
// Scenario support for multi-step operations
// =====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    #[serde(rename = "type")]
    pub scenario_type: String,
    pub steps: Vec<ScenarioStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioStep {
    pub name: String,
    pub method: String,
    pub endpoint: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub extract_response: HashMap<String, String>,
    #[serde(default)]
    pub polling: Option<PollingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollingConfig {
    pub interval_seconds: u64,
    pub timeout_seconds: u64,
    pub completion_conditions: Vec<CompletionCondition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionCondition {
    pub status: String,
    pub action: String,
    #[serde(default)]
    pub error_field: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

// =====================
// Hierarchical mapping
// =====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HierSpec {
    #[serde(default)]
    pub common_args: HashMap<String, ArgSpec>,
    pub commands: Vec<CommandGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandGroup {
    pub name: String,
    #[serde(default)]
    pub about: Option<String>,
    #[serde(default)]
    pub common_args: HashMap<String, ArgSpec>,
    #[serde(default)]
    pub subcommands: Vec<CommandNode>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum CommandNode {
    Command(CommandSpec),
    Group(CommandGroup),
}

impl<'de> serde::Deserialize<'de> for CommandNode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize to a generic Value first to inspect the fields
        let value = serde_yaml::Value::deserialize(deserializer)?;

        // Check if it has subcommands -> Group
        if let serde_yaml::Value::Mapping(ref map) = value {
            let has_subcommands = map.iter().any(|(k, _)| {
                if let serde_yaml::Value::String(s) = k {
                    s == "subcommands"
                } else {
                    false
                }
            });

            if has_subcommands {
                let group: CommandGroup = serde_yaml::from_value(value)
                    .map_err(serde::de::Error::custom)?;
                return Ok(CommandNode::Group(group));
            }

            // Check if it has method, endpoint, or scenario -> Command
            let has_command_fields = map.iter().any(|(k, _)| {
                if let serde_yaml::Value::String(s) = k {
                    s == "method" || s == "endpoint" || s == "scenario"
                } else {
                    false
                }
            });

            if has_command_fields {
                let command: CommandSpec = serde_yaml::from_value(value)
                    .map_err(serde::de::Error::custom)?;
                return Ok(CommandNode::Command(command));
            }
        }

        // Default to Command for backward compatibility
        let command: CommandSpec = serde_yaml::from_value(value)
            .map_err(serde::de::Error::custom)?;
        Ok(CommandNode::Command(command))
    }
}

#[derive(Debug, Clone)]
pub enum MappingRoot {
    Hier(HierSpec),
    Flat(FlatSpec),
}

/// Load flat mapping spec from YAML string.
pub fn parse_flat_spec(yaml: &str) -> Result<FlatSpec> {
    let spec: FlatSpec = serde_yaml::from_str(yaml).context("Failed to parse mapping YAML")?;
    Ok(spec)
}

pub fn parse_mapping_root(yaml: &str) -> Result<MappingRoot> {
    // Peek to see if this is hierarchical (has top-level 'commands')
    let val: serde_yaml::Value = serde_yaml::from_str(yaml).context("Invalid YAML")?;
    if val.get("commands").is_some() {
        let spec: HierSpec = serde_yaml::from_value(val).context("Failed to parse hierarchical mapping")?;
        Ok(MappingRoot::Hier(spec))
    } else {
        let spec: FlatSpec = serde_yaml::from_str(yaml).context("Failed to parse flat mapping")?;
        Ok(MappingRoot::Flat(spec))
    }
}

pub fn is_placeholder(token: &str) -> bool {
    token.starts_with('{') && token.ends_with('}') && token.len() >= 3
}

pub fn derive_args_from_pattern(pattern: &str) -> Vec<ArgSpec> {
    let mut args: Vec<ArgSpec> = Vec::new();
    for tok in pattern.split_whitespace() {
        if is_placeholder(tok) {
            let name = tok.trim_start_matches('{').trim_end_matches('}').to_string();
                            args.push(ArgSpec {
                    name: Some(name.clone()),
                    help: None,
                    positional: Some(true),
                    long: Some(name),
                    short: None,
                    required: Some(true),
                    default: None,
                    arg_type: None,
                    value: None,
                    file_upload: false,
                    file_overrides_value_of: None,
                    inherit: None,
                    endpoint: None,
                    method: None,
                    headers: None,
                    body: None,
                });
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== is_placeholder tests ====================

    #[test]
    fn test_is_placeholder_valid() {
        assert!(is_placeholder("{id}"));
        assert!(is_placeholder("{user_name}"));
        assert!(is_placeholder("{abc}"));
    }

    #[test]
    fn test_is_placeholder_invalid() {
        assert!(!is_placeholder("{}"));
        assert!(!is_placeholder("{"));
        assert!(!is_placeholder("}"));
        assert!(!is_placeholder("id"));
        assert!(!is_placeholder("{a"));
        assert!(!is_placeholder("a}"));
        assert!(!is_placeholder(""));
    }

    // ==================== derive_args_from_pattern tests ====================

    #[test]
    fn test_derive_args_single_placeholder() {
        let args = derive_args_from_pattern("users get {id}");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].name, Some("id".to_string()));
        assert_eq!(args[0].positional, Some(true));
        assert_eq!(args[0].required, Some(true));
    }

    #[test]
    fn test_derive_args_multiple_placeholders() {
        let args = derive_args_from_pattern("users {org} get {id}");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].name, Some("org".to_string()));
        assert_eq!(args[1].name, Some("id".to_string()));
    }

    #[test]
    fn test_derive_args_no_placeholders() {
        let args = derive_args_from_pattern("users list");
        assert!(args.is_empty());
    }

    #[test]
    fn test_derive_args_empty_pattern() {
        let args = derive_args_from_pattern("");
        assert!(args.is_empty());
    }

    // ==================== parse_flat_spec tests ====================

    #[test]
    fn test_parse_flat_spec_minimal() {
        let yaml = r#"
commands:
  - pattern: "users list"
    method: GET
    endpoint: /users
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        assert_eq!(spec.commands.len(), 1);
        assert_eq!(spec.commands[0].pattern, "users list");
        assert_eq!(spec.commands[0].method, Some("GET".to_string()));
        assert_eq!(spec.commands[0].endpoint, Some("/users".to_string()));
    }

    #[test]
    fn test_parse_flat_spec_with_args() {
        let yaml = r#"
commands:
  - pattern: "users get {id}"
    method: GET
    endpoint: /users/{id}
    args:
      - name: id
        help: "User ID"
        required: true
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        assert_eq!(spec.commands[0].args.len(), 1);
        assert_eq!(spec.commands[0].args[0].name, Some("id".to_string()));
        assert_eq!(spec.commands[0].args[0].help, Some("User ID".to_string()));
        assert_eq!(spec.commands[0].args[0].required, Some(true));
    }

    #[test]
    fn test_parse_flat_spec_with_headers() {
        let yaml = r#"
commands:
  - pattern: "api call"
    method: POST
    endpoint: /api
    headers:
      Authorization: "Bearer {token}"
      Content-Type: "application/json"
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        assert_eq!(spec.commands[0].headers.len(), 2);
        assert_eq!(
            spec.commands[0].headers.get("Authorization"),
            Some(&"Bearer {token}".to_string())
        );
    }

    #[test]
    fn test_parse_flat_spec_with_body() {
        let yaml = r#"
commands:
  - pattern: "users create"
    method: POST
    endpoint: /users
    body: '{"name": "{name}", "email": "{email}"}'
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        assert!(spec.commands[0].body.is_some());
        assert!(spec.commands[0].body.as_ref().unwrap().contains("{name}"));
    }

    #[test]
    fn test_parse_flat_spec_with_custom_handler() {
        let yaml = r#"
commands:
  - pattern: "export users"
    custom_handler: export_users
    args:
      - name: format
        default: json
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        assert_eq!(spec.commands[0].custom_handler, Some("export_users".to_string()));
    }

    #[test]
    fn test_parse_flat_spec_invalid_yaml() {
        let yaml = "not: valid: yaml: [";
        let result = parse_flat_spec(yaml);
        assert!(result.is_err());
    }

    // ==================== parse_mapping_root tests ====================

    #[test]
    fn test_parse_mapping_root_detects_hierarchical() {
        let yaml = r#"
commands:
  - name: users
    about: "User management"
    subcommands:
      - name: list
        method: GET
        endpoint: /users
"#;
        let root = parse_mapping_root(yaml).unwrap();
        assert!(matches!(root, MappingRoot::Hier(_)));
    }

    #[test]
    fn test_parse_mapping_root_without_commands_key() {
        // parse_mapping_root falls back to flat when no 'commands' key
        // but FlatSpec requires 'commands', so this tests the fallback path error
        let yaml = r#"
other_key: value
"#;
        let result = parse_mapping_root(yaml);
        assert!(result.is_err()); // Falls back to flat but fails since no 'commands'
    }

    #[test]
    fn test_parse_flat_spec_directly() {
        // Flat specs with 'commands' should use parse_flat_spec directly
        let yaml = r#"
commands:
  - pattern: "users list"
    method: GET
    endpoint: /users
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        assert_eq!(spec.commands.len(), 1);
    }

    #[test]
    fn test_parse_hierarchical_nested_groups() {
        let yaml = r#"
commands:
  - name: org
    about: "Organization commands"
    subcommands:
      - name: users
        about: "User commands"
        subcommands:
          - name: list
            method: GET
            endpoint: /org/{org_id}/users
"#;
        let root = parse_mapping_root(yaml).unwrap();
        if let MappingRoot::Hier(spec) = root {
            assert_eq!(spec.commands.len(), 1);
            assert_eq!(spec.commands[0].name, "org");
            assert_eq!(spec.commands[0].subcommands.len(), 1);
        } else {
            panic!("Expected hierarchical spec");
        }
    }

    #[test]
    fn test_parse_hierarchical_with_common_args() {
        let yaml = r#"
common_args:
  verbose:
    name: verbose
    type: bool
    help: "Enable verbose output"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users
        use_common_args:
          - verbose
"#;
        let root = parse_mapping_root(yaml).unwrap();
        if let MappingRoot::Hier(spec) = root {
            assert!(spec.common_args.contains_key("verbose"));
        } else {
            panic!("Expected hierarchical spec");
        }
    }

    // ==================== CommandNode deserialization tests ====================

    #[test]
    fn test_command_node_deserialize_as_command() {
        let yaml = r#"
name: list
method: GET
endpoint: /users
"#;
        let node: CommandNode = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(node, CommandNode::Command(_)));
    }

    #[test]
    fn test_command_node_deserialize_as_group() {
        let yaml = r#"
name: users
about: "User commands"
subcommands:
  - name: list
    method: GET
    endpoint: /users
"#;
        let node: CommandNode = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(node, CommandNode::Group(_)));
    }

    #[test]
    fn test_command_node_deserialize_with_scenario() {
        let yaml = r#"
name: deploy
scenario:
  type: sequential
  steps:
    - name: step1
      method: POST
      endpoint: /deploy
"#;
        let node: CommandNode = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(node, CommandNode::Command(_)));
        if let CommandNode::Command(cmd) = node {
            assert!(cmd.scenario.is_some());
        }
    }

    #[test]
    fn test_command_node_deserialize_default_to_command() {
        // When no method/endpoint/scenario/subcommands, should default to Command
        let yaml = r#"
name: test
pattern: "test cmd"
"#;
        let node: CommandNode = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(node, CommandNode::Command(_)));
    }

    // ==================== MappingRoot tests ====================

    #[test]
    fn test_mapping_root_debug() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let debug_str = format!("{:?}", root);
        assert!(debug_str.contains("Hier"));
    }

    #[test]
    fn test_mapping_root_flat_debug() {
        let yaml = r#"
commands:
  - pattern: "test cmd"
    method: GET
    endpoint: /test
"#;
        let flat = parse_flat_spec(yaml).unwrap();
        let root = MappingRoot::Flat(flat);
        let debug_str = format!("{:?}", root);
        assert!(debug_str.contains("Flat"));
    }

    // ==================== Scenario parsing tests ====================

    #[test]
    fn test_parse_command_with_scenario() {
        let yaml = r#"
commands:
  - pattern: "deploy"
    scenario:
      type: sequential
      steps:
        - name: create
          method: POST
          endpoint: /deployments
          extract_response:
            deployment_id: "$.id"
        - name: wait
          method: GET
          endpoint: /deployments/{deployment_id}
          polling:
            interval_seconds: 5
            timeout_seconds: 300
            completion_conditions:
              - status: completed
                action: success
              - status: failed
                action: fail
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        let scenario = spec.commands[0].scenario.as_ref().unwrap();
        assert_eq!(scenario.scenario_type, "sequential");
        assert_eq!(scenario.steps.len(), 2);
        assert_eq!(scenario.steps[0].name, "create");
        assert!(scenario.steps[0].extract_response.contains_key("deployment_id"));
        assert!(scenario.steps[1].polling.is_some());
    }

    // ==================== ArgSpec with conditional values ====================

    #[test]
    fn test_parse_arg_with_conditional_value_mapping() {
        let yaml = r#"
commands:
  - pattern: "users list"
    method: GET
    endpoint: /users
    args:
      - name: verbose
        type: bool
        value:
          if_set: "true"
          if_not_set: "false"
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        let arg = &spec.commands[0].args[0];
        assert!(arg.value.is_some());
        if let Some(ConditionalValue::Mapping { if_set, if_not_set }) = &arg.value {
            assert_eq!(if_set, &Some("true".to_string()));
            assert_eq!(if_not_set, &Some("false".to_string()));
        } else {
            panic!("Expected ConditionalValue::Mapping");
        }
    }

    // ==================== Table view parsing ====================

    #[test]
    fn test_parse_command_with_table_view() {
        let yaml = r#"
commands:
  - pattern: "users list"
    method: GET
    endpoint: /users
    table_view:
      - id
      - name
      - email
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        let table_view = spec.commands[0].table_view.as_ref().unwrap();
        assert_eq!(table_view.len(), 3);
        assert_eq!(table_view[0], "id");
    }

    // ==================== File override parsing ====================

    #[test]
    fn test_parse_arg_with_file_override() {
        let yaml = r#"
commands:
  - pattern: "config update"
    method: PUT
    endpoint: /config
    body: "{config_content}"
    args:
      - name: config_file
        type: file
        file-overrides-value-of: config_content
        help: "Path to config file"
"#;
        let spec = parse_flat_spec(yaml).unwrap();
        let arg = &spec.commands[0].args[0];
        assert_eq!(arg.arg_type, Some("file".to_string()));
        assert_eq!(arg.file_overrides_value_of, Some("config_content".to_string()));
    }
}
