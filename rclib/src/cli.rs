use std::collections::{HashMap, HashSet};

use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::mapping::*;
use crate::{
    build_request_from_command, execute_requests_loop, ExecutionConfig, OutputFormat,
    RawRequestSpec, RequestSpec,
};

#[derive(Default)]
struct TreeNode {
    children: HashMap<String, TreeNode>,
    args: Vec<ArgSpec>,
    about: Option<String>,
}

/// FIXME: Memory leak for 'static lifetime requirement
///
/// Clap's builder API requires `'static` strings for `Command::new()`, `Arg::new()`,
/// `.long()`, `.about()`, and `.default_value()`. This is a hard requirement in clap's
/// type signatures - the compiler error is: "argument requires that 'a must outlive 'static".
///
/// Since our CLI is built dynamically from runtime YAML parsing, the command/arg names
/// and descriptions are owned `String`s that only live during `build_cli()` execution.
/// We must leak them to satisfy clap's `'static` requirement.
///
/// Why this is acceptable:
/// 1. CLI building happens once at program startup
/// 2. The leaked memory is small (~10KB for typical command structures)
/// 3. CLI programs are short-lived and exit shortly after argument parsing
/// 4. Alternative approaches (leaking entire `MappingRoot`, using thread-local storage)
///    would leak similar amounts of memory with added complexity
fn leak_str<S: Into<String>>(s: S) -> &'static str {
    Box::leak(s.into().into_boxed_str())
}

pub fn build_cli(
    mapping_root: &MappingRoot,
    default_base_url: &str,
) -> (Command, HashMap<Vec<String>, CommandSpec>) {
    // Build a tree of commands from mapping patterns
    let mut root = TreeNode::default();
    let mut leaf_map: HashMap<Vec<String>, CommandSpec> = HashMap::new();

    match mapping_root {
        MappingRoot::Flat(flat) => {
            for cmd in &flat.commands {
                let tokens: Vec<&str> = cmd.pattern.split_whitespace().collect();
                let path_tokens: Vec<&str> = tokens
                    .iter()
                    .cloned()
                    .filter(|t| !is_placeholder(t))
                    .collect();
                let mut node = &mut root;
                for pt in &path_tokens {
                    node = node.children.entry((*pt).to_string()).or_default();
                }
                node.args = if cmd.args.is_empty() {
                    derive_args_from_pattern(&cmd.pattern)
                } else {
                    cmd.args.clone()
                };
                leaf_map.insert(
                    path_tokens.iter().map(|s| s.to_string()).collect(),
                    cmd.clone(),
                );
            }
        }
        MappingRoot::Hier(hier) => {
            fn ensure_path<'a>(mut node: &'a mut TreeNode, path: &[String]) -> &'a mut TreeNode {
                for seg in path {
                    node = node.children.entry(seg.clone()).or_default();
                }
                node
            }
            fn merge_arg_specs(base: &ArgSpec, override_spec: &ArgSpec) -> ArgSpec {
                ArgSpec {
                    name: override_spec.name.clone().or_else(|| base.name.clone()),
                    inherit: None,
                    help: override_spec.help.clone().or_else(|| base.help.clone()),
                    positional: override_spec.positional.or(base.positional),
                    long: override_spec.long.clone().or_else(|| base.long.clone()),
                    short: override_spec.short.clone().or_else(|| base.short.clone()),
                    required: override_spec.required.or(base.required),
                    default: override_spec
                        .default
                        .clone()
                        .or_else(|| base.default.clone()),
                    arg_type: override_spec
                        .arg_type
                        .clone()
                        .or_else(|| base.arg_type.clone()),
                    value: override_spec.value.clone().or_else(|| base.value.clone()),
                    file_upload: override_spec.file_upload || base.file_upload,
                    endpoint: override_spec
                        .endpoint
                        .clone()
                        .or_else(|| base.endpoint.clone()),
                    method: override_spec.method.clone().or_else(|| base.method.clone()),
                    headers: override_spec
                        .headers
                        .clone()
                        .or_else(|| base.headers.clone()),
                    body: override_spec.body.clone().or_else(|| base.body.clone()),
                    file_overrides_value_of: override_spec
                        .file_overrides_value_of
                        .clone()
                        .or_else(|| base.file_overrides_value_of.clone()),
                }
            }
            fn walk_group(
                root: &mut TreeNode,
                leaf_map: &mut HashMap<Vec<String>, CommandSpec>,
                path: &mut Vec<String>,
                group: &CommandGroup,
                top_level_common_args: &HashMap<String, ArgSpec>,
            ) {
                path.push(group.name.clone());
                // Ensure group node exists and set its about
                let group_node = ensure_path(root, path);
                if let Some(a) = &group.about {
                    group_node.about = Some(a.clone());
                }

                for node in &group.subcommands {
                    match node {
                        CommandNode::Group(g) => {
                            walk_group(root, leaf_map, path, g, top_level_common_args);
                        }
                        CommandNode::Command(cmd) => {
                            let mut insert_path = path.clone();
                            let cmd_name = cmd.name.clone().unwrap_or_else(|| {
                                if !cmd.pattern.is_empty() {
                                    cmd.pattern
                                        .split_whitespace()
                                        .filter(|t| !is_placeholder(t))
                                        .next_back()
                                        .unwrap_or("")
                                        .to_string()
                                } else {
                                    String::new()
                                }
                            });
                            if !cmd_name.is_empty() {
                                insert_path.push(cmd_name);
                            }
                            let node_ref = ensure_path(root, &insert_path);

                            // Resolve args with inheritance from group's common_args
                            let mut resolved_args: Vec<ArgSpec> = Vec::new();
                            if !cmd.args.is_empty() {
                                for a in &cmd.args {
                                    if let Some(inherit_key) = &a.inherit {
                                        // First check group-level common_args, then top-level
                                        let base = group
                                            .common_args
                                            .get(inherit_key)
                                            .or_else(|| top_level_common_args.get(inherit_key));

                                        if let Some(base) = base {
                                            let merged = merge_arg_specs(base, a);
                                            resolved_args.push(merged);
                                        } else {
                                            // No base found; use override as-is
                                            resolved_args.push(a.clone());
                                        }
                                    } else {
                                        resolved_args.push(a.clone());
                                    }
                                }
                            } else {
                                resolved_args = derive_args_from_pattern(&cmd.pattern);
                            }

                            // Add common args requested via legacy use_common_args
                            for common_arg_name in &cmd.use_common_args {
                                if let Some(common_arg) = group.common_args.get(common_arg_name) {
                                    resolved_args.push(common_arg.clone());
                                }
                            }

                            node_ref.args = resolved_args;
                            if let Some(a) = &cmd.about {
                                node_ref.about = Some(a.clone());
                            }

                            // Create a resolved command for the leaf map
                            let mut resolved_cmd = cmd.clone();
                            resolved_cmd.args = node_ref.args.clone();
                            leaf_map.insert(insert_path, resolved_cmd);
                        }
                    }
                }
                path.pop();
            }
            for g in &hier.commands {
                walk_group(
                    &mut root,
                    &mut leaf_map,
                    &mut Vec::new(),
                    g,
                    &hier.common_args,
                );
            }
        }
    }

    // Build clap::Command recursively
    let mut app = Command::new("hscli")
        .about("Hyperspot REST client driven by OpenAPI and YAML mappings")
        .version(env!("CARGO_PKG_VERSION"))
        // Global options
        .arg(
            Arg::new("log-file")
                .long("log-file")
                .short('l')
                .help("Path to log file (JSON format)")
                .num_args(1),
        )
        .arg(
            Arg::new("base-url")
                .long("base-url")
                .short('u')
                .help("Base API URL")
                .num_args(1)
                .default_value(leak_str(default_base_url.to_string())),
        )
        .arg(
            Arg::new("json-output")
                .long("json-output")
                .short('j')
                .help("Output in JSON format")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .short('v')
                .help("Verbose output")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("conn-timeout")
                .long("conn-timeout")
                .help("Connection timeout in seconds")
                .default_value("30")
                .num_args(1),
        )
        .arg(
            Arg::new("timeout")
                .long("timeout")
                .short('t')
                .help("Request timeout in seconds (after connection)")
                .default_value("300")
                .num_args(1),
        )
        .arg(
            Arg::new("openapi-file")
                .long("openapi-file")
                .help("Path to OpenAPI spec file")
                .num_args(1),
        )
        .arg(
            Arg::new("mapping-file")
                .long("mapping-file")
                .help("Path to mapping YAML file")
                .num_args(1),
        )
        // Performance testing options
        .next_help_heading("Perf tests options")
        .arg(
            Arg::new("count")
                .long("count")
                .short('n')
                .help("Execute given command N times")
                .default_value("1")
                .value_parser(clap::value_parser!(u32)),
        )
        .arg(
            Arg::new("duration")
                .long("duration")
                .short('d')
                .help("Execute requests for N seconds (overrides --count)")
                .num_args(1)
                .value_parser(clap::value_parser!(u32))
                .default_value("0"),
        )
        .arg(
            Arg::new("concurrency")
                .long("concurrency")
                .short('c')
                .help("Parallel execution concurrency")
                .num_args(1)
                .value_parser(clap::value_parser!(u32))
                .default_value("1"),
        );

    // Add 'raw' command
    let raw_cmd = Command::new("raw")
        .about("Execute raw HTTP request")
        .arg(
            Arg::new("method")
                .long("method")
                .help("HTTP method")
                .required(true)
                .num_args(1),
        )
        .arg(
            Arg::new("endpoint")
                .long("endpoint")
                .help("Endpoint path or absolute URL")
                .required(true)
                .num_args(1),
        )
        .arg(
            Arg::new("header")
                .long("header")
                .short('H')
                .help("Header 'Key: Value' (repeatable)")
                .num_args(1)
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("body")
                .long("body")
                .help("Request body")
                .num_args(1),
        );
    app = app.subcommand(raw_cmd);

    // Add hierarchical commands
    app = add_children_commands(app, Vec::new(), &root);

    (app, leaf_map)
}

fn add_children_commands(mut app: Command, path: Vec<String>, node: &TreeNode) -> Command {
    // Add children of current node under the app
    for (name, child) in &node.children {
        let mut cmd = Command::new(leak_str(name.clone()));
        if let Some(about) = &child.about {
            cmd = cmd.about(leak_str(about.clone()));
        }
        // If this node represents a concrete command (has args), attach its args
        if !child.args.is_empty() {
            // Add args (positional first, then flags)
            let mut pos_index: usize = 1;
            for arg in &child.args {
                if arg.positional.unwrap_or(false) {
                    let id = arg.long.as_deref().or(arg.name.as_deref()).unwrap_or("");
                    if !id.is_empty() {
                        let mut a = Arg::new(leak_str(id.to_string()))
                            .help(arg.help.clone().unwrap_or_default())
                            .required(arg.required.unwrap_or(false))
                            .num_args(1)
                            .index(pos_index);
                        if let Some(def) = &arg.default {
                            a = a.default_value(leak_str(def.clone()));
                        }
                        cmd = cmd.arg(a);
                        pos_index += 1;
                    }
                }
            }
            for arg in &child.args {
                if !arg.positional.unwrap_or(false) {
                    let id = arg.long.as_deref().or(arg.name.as_deref()).unwrap_or("");
                    if !id.is_empty() {
                        let mut a = Arg::new(leak_str(id.to_string()))
                            .help(arg.help.clone().unwrap_or_default())
                            .required(arg.required.unwrap_or(false));

                        // Handle boolean flags differently
                        if arg.arg_type.as_deref() == Some("bool") {
                            a = a.action(ArgAction::SetTrue);
                        } else {
                            a = a.num_args(1);
                            if let Some(def) = &arg.default {
                                a = a.default_value(leak_str(def.clone()));
                            }
                        }

                        if let Some(l) = arg.long.as_deref() {
                            a = a.long(leak_str(l.to_string()));
                        } else if let Some(n) = arg.name.as_deref() {
                            a = a.long(leak_str(n.to_string()));
                        }
                        if let Some(s) = arg.short.as_deref() {
                            a = a.short(s.chars().next().unwrap());
                        }
                        cmd = cmd.arg(a);
                    }
                }
            }
        }

        // Recurse
        let mut new_path = path.clone();
        new_path.push(name.clone());
        let nested = add_children_subcommands(cmd, new_path, child);
        app = app.subcommand(nested);
    }
    app
}

fn add_children_subcommands(mut cmd: Command, path: Vec<String>, node: &TreeNode) -> Command {
    for (name, child) in &node.children {
        let mut sub = Command::new(leak_str(name.clone()));
        if let Some(about) = &child.about {
            sub = sub.about(leak_str(about.clone()));
        }
        if !child.args.is_empty() {
            let mut pos_index: usize = 1;
            for arg in &child.args {
                if arg.positional.unwrap_or(false) {
                    let id = arg.long.as_deref().or(arg.name.as_deref()).unwrap_or("");
                    if !id.is_empty() {
                        let mut a = Arg::new(leak_str(id.to_string()))
                            .help(arg.help.clone().unwrap_or_default())
                            .required(arg.required.unwrap_or(false))
                            .num_args(1)
                            .index(pos_index);
                        if let Some(def) = &arg.default {
                            a = a.default_value(leak_str(def.clone()));
                        }
                        sub = sub.arg(a);
                        pos_index += 1;
                    }
                }
            }
            for arg in &child.args {
                if !arg.positional.unwrap_or(false) {
                    let id = arg.long.as_deref().or(arg.name.as_deref()).unwrap_or("");
                    if !id.is_empty() {
                        let mut a = Arg::new(leak_str(id.to_string()))
                            .help(arg.help.clone().unwrap_or_default())
                            .required(arg.required.unwrap_or(false));

                        // Handle boolean flags differently
                        if arg.arg_type.as_deref() == Some("bool") {
                            a = a.action(ArgAction::SetTrue);
                        } else {
                            a = a.num_args(1);
                            if let Some(def) = &arg.default {
                                a = a.default_value(leak_str(def.clone()));
                            }
                        }

                        if let Some(l) = arg.long.as_deref() {
                            a = a.long(leak_str(l.to_string()));
                        } else if let Some(n) = arg.name.as_deref() {
                            a = a.long(leak_str(n.to_string()));
                        }
                        if let Some(s) = arg.short.as_deref() {
                            a = a.short(s.chars().next().unwrap());
                        }
                        sub = sub.arg(a);
                    }
                }
            }
        }
        let mut new_path = path.clone();
        new_path.push(name.clone());
        let nested = add_children_subcommands(sub, new_path, child);
        cmd = cmd.subcommand(nested);
    }
    cmd
}

pub fn collect_subcommand_path(matches: &ArgMatches) -> (Vec<String>, &ArgMatches) {
    let mut path: Vec<String> = Vec::new();
    let mut current = matches;
    while let Some((name, sub_m)) = current.subcommand() {
        if name == "raw" {
            break;
        }
        path.push(name.to_string());
        current = sub_m;
        if sub_m.subcommand().is_none() {
            break;
        }
    }
    (path, current)
}

pub fn print_manual_help(path: &[String], cmd: &CommandSpec) {
    // NAME
    let prog = "hscli";
    let title = cmd.about.clone().unwrap_or_default();
    if title.is_empty() {
        println!("{} {} -", prog, path.join(" "));
    } else {
        println!("{} {} - {}", prog, path.join(" "), title);
    }

    // USAGE
    println!("\nUSAGE:\n  {} {} [command options]", prog, path.join(" "));

    // OPTIONS
    println!("\nOPTIONS:");
    for arg in &cmd.args {
        let long = arg
            .long
            .clone()
            .or_else(|| arg.name.clone())
            .unwrap_or_default();
        let help = arg.help.clone().unwrap_or_default();
        let required = arg.required.unwrap_or(false);
        println!(
            "  --{} value  {}{}",
            long,
            help,
            if required { " (required)" } else { "" }
        );
    }
    println!("  --help, -h       show help");
}

pub fn pre_scan_value(args: &[String], key: &str) -> Option<String> {
    for i in 0..args.len() {
        if args[i] == key && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        if let Some(rest) = args[i].strip_prefix(&(key.to_string() + "=")) {
            return Some(rest.to_string());
        }
    }
    None
}

// =====================
// Runtime helpers
// =====================

pub type CustomHandlerFn =
    dyn Fn(&HashMap<String, String>, &str, bool) -> anyhow::Result<()> + Send + Sync + 'static;

#[derive(Default)]
pub struct HandlerRegistry {
    handlers: HashMap<String, Box<CustomHandlerFn>>,
}

impl HandlerRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&HashMap<String, String>, &str, bool) -> anyhow::Result<()> + Send + Sync + 'static,
    {
        self.handlers.insert(name.to_string(), Box::new(f));
    }
    pub fn get(&self, name: &str) -> Option<&CustomHandlerFn> {
        self.handlers.get(name).map(AsRef::as_ref)
    }
}

pub fn validate_handlers(root: &MappingRoot, registry: &HandlerRegistry) -> anyhow::Result<()> {
    let mut missing: Vec<String> = Vec::new();
    match root {
        MappingRoot::Flat(flat) => {
            for cmd in &flat.commands {
                if let Some(h) = &cmd.custom_handler {
                    if !registry.handlers.contains_key(h) {
                        missing.push(h.clone());
                    }
                }
            }
        }
        MappingRoot::Hier(hier) => {
            fn walk(group: &CommandGroup, reg: &HandlerRegistry, acc: &mut Vec<String>) {
                for node in &group.subcommands {
                    match node {
                        CommandNode::Group(g) => walk(g, reg, acc),
                        CommandNode::Command(c) => {
                            if let Some(h) = &c.custom_handler {
                                if !reg.handlers.contains_key(h) {
                                    acc.push(h.clone());
                                }
                            }
                        }
                    }
                }
            }
            for g in &hier.commands {
                walk(g, registry, &mut missing);
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Missing custom handlers: {}",
            missing.join(", ")
        ))
    }
}

fn parse_timeout(matches: &ArgMatches, arg_name: &str) -> Option<f64> {
    matches
        .get_one::<String>(arg_name)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| *v >= 0.0)
}

pub fn collect_vars_from_matches(
    cmd: &CommandSpec,
    leaf: &ArgMatches,
) -> (HashMap<String, String>, HashSet<String>, bool) {
    let arg_specs: Vec<ArgSpec> = if cmd.args.is_empty() {
        derive_args_from_pattern(&cmd.pattern)
    } else {
        cmd.args.clone()
    };
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut selected: HashSet<String> = HashSet::new();
    let mut missing_required = false;
    for arg in &arg_specs {
        let name = arg
            .long
            .clone()
            .or_else(|| arg.name.clone())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        if arg.arg_type.as_deref() == Some("bool") {
            let is_set = leaf.get_flag(&name);
            if let Some(var_name) = arg.name.clone() {
                if is_set {
                    selected.insert(var_name.clone());
                }
                if let Some(cond) = &arg.value {
                    let val = match cond {
                        ConditionalValue::Mapping { if_set, if_not_set } => {
                            if is_set {
                                if_set.clone()
                            } else {
                                if_not_set.clone()
                            }
                        }
                        ConditionalValue::Sequence(entries) => {
                            if is_set {
                                entries.iter().find_map(|e| e.if_set.clone())
                            } else {
                                entries.iter().find_map(|e| e.if_not_set.clone())
                            }
                        }
                    }
                    .unwrap_or_else(|| {
                        if is_set {
                            "true".to_string()
                        } else {
                            "false".to_string()
                        }
                    });
                    vars.insert(var_name, val);
                } else {
                    vars.insert(
                        var_name,
                        if is_set {
                            "true".to_string()
                        } else {
                            "false".to_string()
                        },
                    );
                }
            }
        } else if let Some(val) = leaf.get_one::<String>(&name) {
            if let Some(var_name) = arg.name.clone() {
                vars.insert(var_name.clone(), val.clone());
                selected.insert(var_name);
            }
        } else if let Some(def) = &arg.default {
            if let Some(var_name) = arg.name.clone() {
                vars.insert(var_name, def.clone());
            }
        } else if arg.required.unwrap_or(false) {
            missing_required = true;
        }
    }
    (vars, selected, missing_required)
}

pub fn drive_command(
    root: &MappingRoot,
    default_base_url: &str,
    matches: &ArgMatches,
    handlers: &HandlerRegistry,
    user_agent: &str,
) -> anyhow::Result<i32> {
    let base_url = matches
        .get_one::<String>("base-url")
        .cloned()
        .unwrap_or_else(|| default_base_url.to_string());
    let json_output = matches.get_flag("json-output");
    let verbose = matches.get_flag("verbose");

    let config = ExecutionConfig {
        output: if json_output {
            OutputFormat::Json
        } else {
            OutputFormat::Human
        },
        conn_timeout_secs: parse_timeout(matches, "conn-timeout"),
        request_timeout_secs: parse_timeout(matches, "timeout"),
        user_agent,
        verbose,
        count: matches.get_one::<u32>("count").copied(),
        duration_secs: matches.get_one::<u32>("duration").copied().unwrap_or(0),
        concurrency: matches.get_one::<u32>("concurrency").copied().unwrap_or(1),
    };

    // RAW subcommand handled here
    if let Some(("raw", raw_m)) = matches.subcommand() {
        let method = raw_m
            .get_one::<String>("method")
            .cloned()
            .unwrap_or_else(|| "GET".to_string());
        let endpoint = raw_m
            .get_one::<String>("endpoint")
            .cloned()
            .unwrap_or_default();
        let headers: Vec<String> = raw_m
            .get_many::<String>("header")
            .map(|v| v.cloned().collect())
            .unwrap_or_default();
        let body = raw_m.get_one::<String>("body").cloned();
        let raw_spec = RawRequestSpec {
            base_url: Some(base_url.clone()),
            method,
            endpoint,
            headers,
            body,
            multipart: false,
            file_fields: HashMap::new(),
            table_view: None,
        };
        return execute_requests_loop(&RequestSpec::Simple(raw_spec), &config);
    }

    // Build path->command map and current path
    let (mut app2, path_map) = build_cli(root, default_base_url);
    let (path, leaf) = collect_subcommand_path(matches);
    if path.is_empty() {
        let _ = app2.print_help();
        return Ok(0);
    }

    if let Some(cmd) = path_map.get(&path) {
        let (vars, selected, missing_required) = collect_vars_from_matches(cmd, leaf);
        if missing_required {
            print_manual_help(&path, cmd);
            return Ok(2);
        }
        let spec = build_request_from_command(Some(base_url.clone()), cmd, &vars, &selected);
        match &spec {
            RequestSpec::CustomHandler { handler_name, vars } => {
                let h = handlers
                    .get(handler_name)
                    .ok_or_else(|| anyhow::anyhow!("No handler registered for {}", handler_name))?;
                h(vars, &base_url, json_output)?;
                Ok(0)
            }
            _ => execute_requests_loop(&spec, &config),
        }
    } else {
        // Intermediate path: print nested help
        let mut cmd = app2;
        for name in &path {
            let next_opt = cmd
                .get_subcommands()
                .find(|c| c.get_name() == name)
                .cloned();
            if let Some(next_cmd) = next_opt {
                cmd = next_cmd;
            } else {
                break;
            }
        }
        let _ = cmd.clone().print_help();
        println!();
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== pre_scan_value tests ====================

    #[test]
    fn test_pre_scan_value_space_separated() {
        let args = vec![
            "cli".to_string(),
            "--base-url".to_string(),
            "https://api.example.com".to_string(),
            "users".to_string(),
        ];
        let result = pre_scan_value(&args, "--base-url");
        assert_eq!(result, Some("https://api.example.com".to_string()));
    }

    #[test]
    fn test_pre_scan_value_equals_separated() {
        let args = vec![
            "cli".to_string(),
            "--base-url=https://api.example.com".to_string(),
            "users".to_string(),
        ];
        let result = pre_scan_value(&args, "--base-url");
        assert_eq!(result, Some("https://api.example.com".to_string()));
    }

    #[test]
    fn test_pre_scan_value_not_found() {
        let args = vec!["cli".to_string(), "users".to_string()];
        let result = pre_scan_value(&args, "--base-url");
        assert_eq!(result, None);
    }

    #[test]
    fn test_pre_scan_value_at_end_no_value() {
        let args = vec!["cli".to_string(), "--base-url".to_string()];
        let result = pre_scan_value(&args, "--base-url");
        assert_eq!(result, None); // No value after the key
    }

    #[test]
    fn test_pre_scan_value_empty_args() {
        let args: Vec<String> = vec![];
        let result = pre_scan_value(&args, "--base-url");
        assert_eq!(result, None);
    }

    // ==================== HandlerRegistry tests ====================

    #[test]
    fn test_handler_registry_new() {
        let reg = HandlerRegistry::new();
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn test_handler_registry_register_and_get() {
        let mut reg = HandlerRegistry::new();
        reg.register("test_handler", |_vars, _base_url, _json| Ok(()));
        assert!(reg.get("test_handler").is_some());
        assert!(reg.get("other_handler").is_none());
    }

    // ==================== build_cli tests ====================

    #[test]
    fn test_build_cli_hierarchical_creates_subcommands() {
        let yaml = r#"
commands:
  - name: users
    about: "User management"
    subcommands:
      - name: list
        method: GET
        endpoint: /users
      - name: get
        method: GET
        endpoint: /users/{id}
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, path_map) = build_cli(&root, "https://api.example.com");

        // Verify command structure
        let subcommands: Vec<_> = app.get_subcommands().collect();
        let users_cmd = subcommands.iter().find(|c| c.get_name() == "users");
        assert!(users_cmd.is_some());

        // Verify path map has entries
        assert!(path_map.contains_key(&vec!["users".to_string(), "list".to_string()]));
        assert!(path_map.contains_key(&vec!["users".to_string(), "get".to_string()]));
    }

    #[test]
    fn test_build_cli_adds_global_args() {
        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        // Verify global args exist
        let args: Vec<_> = app.get_arguments().collect();
        let arg_names: Vec<_> = args.iter().map(|a| a.get_id().as_str()).collect();

        assert!(arg_names.contains(&"base-url"));
        assert!(arg_names.contains(&"json-output"));
        assert!(arg_names.contains(&"verbose"));
    }

    #[test]
    fn test_build_cli_nested_groups() {
        let yaml = r#"
commands:
  - name: org
    about: "Organization commands"
    subcommands:
      - name: members
        about: "Member management"
        subcommands:
          - name: list
            method: GET
            endpoint: /org/{org_id}/members
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (_, path_map) = build_cli(&root, "https://api.example.com");

        // Verify deeply nested path
        let path = vec!["org".to_string(), "members".to_string(), "list".to_string()];
        assert!(path_map.contains_key(&path));
    }

    // ==================== validate_handlers tests ====================

    #[test]
    fn test_validate_handlers_all_registered() {
        let yaml = r#"
commands:
  - name: export
    subcommands:
      - name: users
        custom_handler: export_users
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let mut reg = HandlerRegistry::new();
        reg.register("export_users", |_, _, _| Ok(()));

        let result = validate_handlers(&root, &reg);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_handlers_missing() {
        let yaml = r#"
commands:
  - name: export
    subcommands:
      - name: users
        custom_handler: export_users
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let reg = HandlerRegistry::new(); // No handlers registered

        let result = validate_handlers(&root, &reg);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("export_users"));
    }

    // ==================== collect_subcommand_path tests ====================

    #[test]
    fn test_collect_subcommand_path_simple() {
        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");
        let matches = app.try_get_matches_from(["cli", "users", "list"]).unwrap();

        let (path, _leaf) = collect_subcommand_path(&matches);
        assert_eq!(path, vec!["users".to_string(), "list".to_string()]);
    }

    #[test]
    fn test_collect_subcommand_path_empty() {
        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");
        let matches = app.try_get_matches_from(["cli"]).unwrap();

        let (path, _) = collect_subcommand_path(&matches);
        assert!(path.is_empty());
    }

    // ==================== collect_vars_from_matches tests ====================

    #[test]
    fn test_collect_vars_with_defaults() {
        let cmd = CommandSpec {
            name: Some("list".to_string()),
            about: None,
            pattern: "users list".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/users".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![ArgSpec {
                name: Some("limit".to_string()),
                default: Some("10".to_string()),
                required: Some(false),
                ..Default::default()
            }],
            use_common_args: vec![],
        };

        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users
        args:
          - name: limit
            default: "10"
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");
        let matches = app.try_get_matches_from(["cli", "users", "list"]).unwrap();
        let (_, leaf) = collect_subcommand_path(&matches);

        let (vars, _, missing) = collect_vars_from_matches(&cmd, leaf);
        assert!(!missing);
        assert_eq!(vars.get("limit"), Some(&"10".to_string()));
    }

    #[test]
    fn test_collect_vars_with_provided_value() {
        let cmd = CommandSpec {
            name: Some("list".to_string()),
            about: None,
            pattern: "users list".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/users".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![ArgSpec {
                name: Some("limit".to_string()),
                long: Some("limit".to_string()),
                default: Some("10".to_string()),
                required: Some(false),
                ..Default::default()
            }],
            use_common_args: vec![],
        };

        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users
        args:
          - name: limit
            long: limit
            default: "10"
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");
        let matches = app
            .try_get_matches_from(["cli", "users", "list", "--limit", "50"])
            .unwrap();
        let (_, leaf) = collect_subcommand_path(&matches);

        let (vars, selected, _) = collect_vars_from_matches(&cmd, leaf);
        assert_eq!(vars.get("limit"), Some(&"50".to_string()));
        assert!(selected.contains("limit"));
    }

    // ==================== Flat spec handling tests ====================

    #[test]
    fn test_build_cli_flat_spec() {
        let yaml = r#"
commands:
  - pattern: "users list"
    method: GET
    endpoint: /users
  - pattern: "users get {id}"
    method: GET
    endpoint: /users/{id}
"#;
        let flat = parse_flat_spec(yaml).unwrap();
        let root = MappingRoot::Flat(flat);
        let (app, path_map) = build_cli(&root, "https://api.example.com");

        // Verify flat commands are parsed
        assert!(path_map.contains_key(&vec!["users".to_string(), "list".to_string()]));
        assert!(path_map.contains_key(&vec!["users".to_string(), "get".to_string()]));

        // Verify CLI structure
        let subcommands: Vec<_> = app.get_subcommands().collect();
        assert!(subcommands.iter().any(|c| c.get_name() == "users"));
    }

    #[test]
    fn test_build_cli_with_positional_args() {
        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: get
        method: GET
        endpoint: /users/{id}
        args:
          - name: id
            positional: true
            required: true
            help: "User ID"
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        // Should be able to parse positional arg
        let matches = app
            .try_get_matches_from(["cli", "users", "get", "123"])
            .unwrap();
        let (path, leaf) = collect_subcommand_path(&matches);
        assert_eq!(path, vec!["users", "get"]);
        assert_eq!(leaf.get_one::<String>("id"), Some(&"123".to_string()));
    }

    #[test]
    fn test_build_cli_with_short_args() {
        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users
        args:
          - name: limit
            long: limit
            short: l
            default: "10"
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        // Should be able to use short arg
        let matches = app
            .try_get_matches_from(["cli", "users", "list", "-l", "25"])
            .unwrap();
        let (_, leaf) = collect_subcommand_path(&matches);
        assert_eq!(leaf.get_one::<String>("limit"), Some(&"25".to_string()));
    }

    // ==================== leak_str tests ====================

    #[test]
    fn test_leak_str() {
        let leaked = leak_str("test string");
        assert_eq!(leaked, "test string");
    }

    #[test]
    fn test_leak_str_from_string() {
        let s = String::from("dynamic string");
        let leaked = leak_str(s);
        assert_eq!(leaked, "dynamic string");
    }

    // ==================== TreeNode tests ====================

    #[test]
    fn test_tree_node_default() {
        let node = TreeNode::default();
        assert!(node.children.is_empty());
        assert!(node.args.is_empty());
        assert!(node.about.is_none());
    }

    // ==================== print_manual_help tests ====================

    #[test]
    fn test_print_manual_help_with_about() {
        let cmd = CommandSpec {
            name: Some("list".to_string()),
            about: Some("List all users".to_string()),
            pattern: "users list".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/users".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![ArgSpec {
                name: Some("limit".to_string()),
                long: Some("limit".to_string()),
                help: Some("Maximum results".to_string()),
                required: Some(false),
                ..Default::default()
            }],
            use_common_args: vec![],
        };
        // Just verify it doesn't panic
        print_manual_help(&["users".to_string(), "list".to_string()], &cmd);
    }

    #[test]
    fn test_print_manual_help_without_about() {
        let cmd = CommandSpec {
            name: Some("list".to_string()),
            about: None,
            pattern: "users list".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/users".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![],
            use_common_args: vec![],
        };
        // Just verify it doesn't panic
        print_manual_help(&["users".to_string(), "list".to_string()], &cmd);
    }

    #[test]
    fn test_print_manual_help_with_required_arg() {
        let cmd = CommandSpec {
            name: Some("get".to_string()),
            about: None,
            pattern: "users get".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/users/{id}".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![ArgSpec {
                name: Some("id".to_string()),
                long: Some("id".to_string()),
                help: Some("User ID".to_string()),
                required: Some(true),
                ..Default::default()
            }],
            use_common_args: vec![],
        };
        // Just verify it doesn't panic
        print_manual_help(&["users".to_string(), "get".to_string()], &cmd);
    }

    // ==================== parse_timeout tests ====================

    #[test]
    fn test_parse_timeout_valid() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");
        let matches = app
            .try_get_matches_from(["cli", "--timeout", "60", "test", "cmd"])
            .unwrap();
        let timeout = parse_timeout(&matches, "timeout");
        assert_eq!(timeout, Some(60.0));
    }

    #[test]
    fn test_parse_timeout_zero() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");
        let matches = app
            .try_get_matches_from(["cli", "--timeout", "0", "test", "cmd"])
            .unwrap();
        let timeout = parse_timeout(&matches, "timeout");
        assert_eq!(timeout, Some(0.0)); // Zero is valid
    }

    // ==================== collect_vars_from_matches edge cases ====================

    #[test]
    fn test_collect_vars_missing_required() {
        // Test that missing required args are detected
        let cmd = CommandSpec {
            name: Some("get".to_string()),
            about: None,
            pattern: "users get".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/users/{id}".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![ArgSpec {
                name: Some("id".to_string()),
                long: Some("id".to_string()),
                required: Some(true),
                ..Default::default()
            }],
            use_common_args: vec![],
        };

        // Build CLI with the arg defined but not required by clap
        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: get
        method: GET
        endpoint: /users/{id}
        args:
          - name: id
            long: id
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");
        let matches = app.try_get_matches_from(["cli", "users", "get"]).unwrap();
        let (_, leaf) = collect_subcommand_path(&matches);

        let (_, _, missing) = collect_vars_from_matches(&cmd, leaf);
        assert!(missing); // Required arg is missing
    }

    #[test]
    fn test_collect_vars_bool_flag_set() {
        let cmd = CommandSpec {
            name: Some("list".to_string()),
            about: None,
            pattern: "users list".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/users".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![ArgSpec {
                name: Some("verbose".to_string()),
                long: Some("verbose".to_string()),
                arg_type: Some("bool".to_string()),
                value: Some(ConditionalValue::Mapping {
                    if_set: Some("true".to_string()),
                    if_not_set: Some("false".to_string()),
                }),
                ..Default::default()
            }],
            use_common_args: vec![],
        };

        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users
        args:
          - name: verbose
            long: verbose
            type: bool
            value:
              if_set: "true"
              if_not_set: "false"
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        // Test with flag set
        let matches = app
            .clone()
            .try_get_matches_from(["cli", "users", "list", "--verbose"])
            .unwrap();
        let (_, leaf) = collect_subcommand_path(&matches);
        let (vars, _, _) = collect_vars_from_matches(&cmd, leaf);
        assert_eq!(vars.get("verbose"), Some(&"true".to_string()));

        // Test without flag
        let matches = app.try_get_matches_from(["cli", "users", "list"]).unwrap();
        let (_, leaf) = collect_subcommand_path(&matches);
        let (vars, _, _) = collect_vars_from_matches(&cmd, leaf);
        assert_eq!(vars.get("verbose"), Some(&"false".to_string()));
    }

    #[test]
    fn test_collect_vars_derives_from_pattern() {
        // When args is empty, derive_args_from_pattern is used
        let cmd = CommandSpec {
            name: Some("get".to_string()),
            about: None,
            pattern: "users get {id}".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/users/{id}".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![], // Empty - will derive from pattern
            use_common_args: vec![],
        };

        // Use flat spec with pattern which derives args automatically
        let yaml = r#"
commands:
  - pattern: "users get {id}"
    method: GET
    endpoint: /users/{id}
"#;
        let flat = parse_flat_spec(yaml).unwrap();
        let root = MappingRoot::Flat(flat);
        let (app, _) = build_cli(&root, "https://api.example.com");
        let matches = app
            .try_get_matches_from(["cli", "users", "get", "123"])
            .unwrap();
        let (_, leaf) = collect_subcommand_path(&matches);

        let (vars, _, _) = collect_vars_from_matches(&cmd, leaf);
        assert_eq!(vars.get("id"), Some(&"123".to_string()));
    }

    // ==================== validate_handlers with flat spec ====================

    #[test]
    fn test_validate_handlers_flat_spec() {
        let yaml = r#"
commands:
  - pattern: "export users"
    custom_handler: export_users
"#;
        let flat = parse_flat_spec(yaml).unwrap();
        let root = MappingRoot::Flat(flat);

        let mut reg = HandlerRegistry::new();
        reg.register("export_users", |_, _, _| Ok(()));

        let result = validate_handlers(&root, &reg);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_handlers_flat_spec_missing() {
        let yaml = r#"
commands:
  - pattern: "export users"
    custom_handler: missing_handler
"#;
        let flat = parse_flat_spec(yaml).unwrap();
        let root = MappingRoot::Flat(flat);
        let reg = HandlerRegistry::new();

        let result = validate_handlers(&root, &reg);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing_handler"));
    }

    // ==================== arg inheritance tests ====================

    #[test]
    fn test_arg_inheritance_from_common_args() {
        let yaml = r#"
commands:
  - name: api
    common_args:
      output_format:
        name: format
        long: format
        default: "json"
        help: "Output format"
    subcommands:
      - name: call
        method: GET
        endpoint: /api
        args:
          - inherit: output_format
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, path_map) = build_cli(&root, "https://api.example.com");

        let matches = app
            .try_get_matches_from(["cli", "api", "call", "--format", "xml"])
            .unwrap();
        let (path, leaf) = collect_subcommand_path(&matches);
        let cmd = path_map.get(&path).unwrap();
        let (vars, _, _) = collect_vars_from_matches(cmd, leaf);

        assert_eq!(vars.get("format"), Some(&"xml".to_string()));
    }

    // ==================== add_children_commands tests ====================

    #[test]
    fn test_add_children_commands_with_about() {
        let yaml = r#"
commands:
  - name: users
    about: "User management"
    subcommands:
      - name: list
        about: "List all users"
        method: GET
        endpoint: /users
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        let users_cmd = app.get_subcommands().find(|c| c.get_name() == "users");
        assert!(users_cmd.is_some());
        let users = users_cmd.unwrap();
        assert!(users.get_about().is_some());
    }

    #[test]
    fn test_add_children_commands_with_default_values() {
        let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users
        args:
          - name: limit
            long: limit
            default: "100"
          - name: offset
            long: offset
            default: "0"
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        // Verify defaults work
        let matches = app.try_get_matches_from(["cli", "users", "list"]).unwrap();
        let (_, leaf) = collect_subcommand_path(&matches);
        assert_eq!(leaf.get_one::<String>("limit"), Some(&"100".to_string()));
        assert_eq!(leaf.get_one::<String>("offset"), Some(&"0".to_string()));
    }

    // ==================== merge_arg_specs coverage ====================

    #[test]
    fn test_arg_inheritance_with_override() {
        let yaml = r#"
commands:
  - name: api
    common_args:
      base_arg:
        name: param
        long: param
        default: "default_value"
        help: "Base help"
        short: p
    subcommands:
      - name: call
        method: GET
        endpoint: /api
        args:
          - inherit: base_arg
            default: "overridden_value"
            help: "Overridden help"
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, path_map) = build_cli(&root, "https://api.example.com");

        let matches = app.try_get_matches_from(["cli", "api", "call"]).unwrap();
        let (path, leaf) = collect_subcommand_path(&matches);
        let cmd = path_map.get(&path).unwrap();
        let (vars, _, _) = collect_vars_from_matches(cmd, leaf);

        // Should use overridden default
        assert_eq!(vars.get("param"), Some(&"overridden_value".to_string()));
    }

    #[test]
    fn test_arg_inheritance_missing_base() {
        // When inherit references non-existent common_arg, use the arg as-is
        let yaml = r#"
commands:
  - name: api
    subcommands:
      - name: call
        method: GET
        endpoint: /api
        args:
          - inherit: nonexistent
            name: fallback
            long: fallback
            default: "fallback_value"
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, path_map) = build_cli(&root, "https://api.example.com");

        let matches = app.try_get_matches_from(["cli", "api", "call"]).unwrap();
        let (path, leaf) = collect_subcommand_path(&matches);
        let cmd = path_map.get(&path).unwrap();
        let (vars, _, _) = collect_vars_from_matches(cmd, leaf);

        assert_eq!(vars.get("fallback"), Some(&"fallback_value".to_string()));
    }

    // ==================== use_common_args legacy support ====================

    #[test]
    fn test_use_common_args_legacy() {
        let yaml = r#"
commands:
  - name: api
    common_args:
      verbose_arg:
        name: verbose
        long: verbose
        type: bool
        value:
          if_set: "true"
          if_not_set: "false"
    subcommands:
      - name: call
        method: GET
        endpoint: /api
        use_common_args:
          - verbose_arg
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, path_map) = build_cli(&root, "https://api.example.com");

        let matches = app
            .try_get_matches_from(["cli", "api", "call", "--verbose"])
            .unwrap();
        let (path, leaf) = collect_subcommand_path(&matches);
        let cmd = path_map.get(&path).unwrap();
        let (vars, _, _) = collect_vars_from_matches(cmd, leaf);

        assert_eq!(vars.get("verbose"), Some(&"true".to_string()));
    }

    // ==================== command name derivation ====================

    #[test]
    fn test_command_name_from_pattern() {
        // When name is not provided, derive from pattern
        let yaml = r#"
commands:
  - name: users
    subcommands:
      - pattern: "users list-all"
        method: GET
        endpoint: /users
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (_, path_map) = build_cli(&root, "https://api.example.com");

        // Should derive "list-all" from pattern
        assert!(path_map.contains_key(&vec!["users".to_string(), "list-all".to_string()]));
    }

    // ==================== raw command tests ====================

    #[test]
    fn test_raw_command_exists() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        // Verify raw command exists
        let raw_cmd = app.get_subcommands().find(|c| c.get_name() == "raw");
        assert!(raw_cmd.is_some());
    }

    #[test]
    fn test_raw_command_args() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        // Parse raw command - collect_subcommand_path stops at "raw"
        let matches = app
            .try_get_matches_from([
                "cli",
                "raw",
                "--method",
                "POST",
                "--endpoint",
                "/api/test",
                "--header",
                "Content-Type: application/json",
                "--body",
                r#"{"key": "value"}"#,
            ])
            .unwrap();

        // raw subcommand is handled specially - verify we can get the subcommand
        if let Some(("raw", raw_m)) = matches.subcommand() {
            assert_eq!(raw_m.get_one::<String>("method"), Some(&"POST".to_string()));
            assert_eq!(
                raw_m.get_one::<String>("endpoint"),
                Some(&"/api/test".to_string())
            );
        } else {
            panic!("Expected raw subcommand");
        }
    }

    // ==================== global args tests ====================

    #[test]
    fn test_global_args_conn_timeout() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        let matches = app
            .try_get_matches_from(["cli", "--conn-timeout", "45", "test", "cmd"])
            .unwrap();
        let timeout = parse_timeout(&matches, "conn-timeout");
        assert_eq!(timeout, Some(45.0));
    }

    #[test]
    fn test_global_args_json_output() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        let matches = app
            .try_get_matches_from(["cli", "--json-output", "test", "cmd"])
            .unwrap();
        assert!(matches.get_flag("json-output"));
    }

    #[test]
    fn test_global_args_verbose() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        let matches = app
            .try_get_matches_from(["cli", "-v", "test", "cmd"])
            .unwrap();
        assert!(matches.get_flag("verbose"));
    }

    // ==================== perf test args ====================

    #[test]
    fn test_perf_args_count() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        let matches = app
            .try_get_matches_from(["cli", "--count", "100", "test", "cmd"])
            .unwrap();
        assert_eq!(matches.get_one::<u32>("count"), Some(&100));
    }

    #[test]
    fn test_perf_args_duration() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        let matches = app
            .try_get_matches_from(["cli", "--duration", "30", "test", "cmd"])
            .unwrap();
        assert_eq!(matches.get_one::<u32>("duration"), Some(&30));
    }

    #[test]
    fn test_perf_args_concurrency() {
        let yaml = r#"
commands:
  - name: test
    subcommands:
      - name: cmd
        method: GET
        endpoint: /test
"#;
        let root = parse_mapping_root(yaml).unwrap();
        let (app, _) = build_cli(&root, "https://api.example.com");

        let matches = app
            .try_get_matches_from(["cli", "--concurrency", "4", "test", "cmd"])
            .unwrap();
        assert_eq!(matches.get_one::<u32>("concurrency"), Some(&4));
    }
}
