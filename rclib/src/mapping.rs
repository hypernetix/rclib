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
