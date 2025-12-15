use std::collections::{HashMap, HashSet};

use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::mapping::*;
use crate::{build_request_from_command, execute_requests_loop, ExecutionConfig, OutputFormat, RequestSpec, RawRequestSpec};

#[derive(Default)]
struct TreeNode {
    children: HashMap<String, TreeNode>,
    args: Vec<ArgSpec>,
    about: Option<String>,
}

fn leak_str<S: Into<String>>(s: S) -> &'static str {
    Box::leak(s.into().into_boxed_str())
}

pub fn build_cli(mapping_root: &MappingRoot, default_base_url: &str) -> (Command, HashMap<Vec<String>, CommandSpec>) {
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
                node.args = if cmd.args.is_empty() { derive_args_from_pattern(&cmd.pattern) } else { cmd.args.clone() };
                leaf_map.insert(path_tokens.iter().map(|s| s.to_string()).collect(), cmd.clone());
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
                    default: override_spec.default.clone().or_else(|| base.default.clone()),
                    arg_type: override_spec.arg_type.clone().or_else(|| base.arg_type.clone()),
                    value: override_spec.value.clone().or_else(|| base.value.clone()),
                    file_upload: override_spec.file_upload || base.file_upload,
                    endpoint: override_spec.endpoint.clone().or_else(|| base.endpoint.clone()),
                    method: override_spec.method.clone().or_else(|| base.method.clone()),
                    headers: override_spec.headers.clone().or_else(|| base.headers.clone()),
                    body: override_spec.body.clone().or_else(|| base.body.clone()),
                    file_overrides_value_of: override_spec.file_overrides_value_of.clone().or_else(|| base.file_overrides_value_of.clone()),                }
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
                if let Some(a) = &group.about { group_node.about = Some(a.clone()); }

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
                                        let base = group.common_args.get(inherit_key)
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
                            if let Some(a) = &cmd.about { node_ref.about = Some(a.clone()); }

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
                walk_group(&mut root, &mut leaf_map, &mut Vec::new(), g, &hier.common_args);
            }
        }
    }

    // Build clap::Command recursively
    let mut app = Command::new("hscli")
        .about("Hyperspot REST client driven by OpenAPI and YAML mappings")
        .version(env!("CARGO_PKG_VERSION"))
        // Global options
        .arg(Arg::new("log-file").long("log-file").short('l').help("Path to log file (JSON format)").num_args(1))
        .arg(Arg::new("base-url").long("base-url").short('u').help("Base API URL").num_args(1).default_value(leak_str(default_base_url.to_string())))
        .arg(Arg::new("json-output").long("json-output").short('j').help("Output in JSON format").action(ArgAction::SetTrue))
        .arg(Arg::new("verbose").long("verbose").short('v').help("Verbose output").action(ArgAction::SetTrue))
        .arg(Arg::new("conn-timeout").long("conn-timeout").help("Connection timeout in seconds").default_value("30").num_args(1))
        .arg(Arg::new("timeout").long("timeout").short('t').help("Request timeout in seconds (after connection)").default_value("300").num_args(1))
        .arg(Arg::new("openapi-file").long("openapi-file").help("Path to OpenAPI spec file").num_args(1))
        .arg(Arg::new("mapping-file").long("mapping-file").help("Path to mapping YAML file").num_args(1))
        // Performance testing options
        .next_help_heading("Perf tests options")
        .arg(Arg::new("count").long("count").short('n').help("Execute given command N times").default_value("1").value_parser(clap::value_parser!(u32)))
        .arg(Arg::new("duration").long("duration").short('d').help("Execute requests for N seconds (overrides --count)").num_args(1).value_parser(clap::value_parser!(u32)).default_value("0"))
        .arg(Arg::new("concurrency").long("concurrency").short('c').help("Parallel execution concurrency").num_args(1).value_parser(clap::value_parser!(u32)).default_value("1"));

    // Add 'raw' command
    let raw_cmd = Command::new("raw")
        .about("Execute raw HTTP request")
        .arg(Arg::new("method").long("method").help("HTTP method").required(true).num_args(1))
        .arg(Arg::new("endpoint").long("endpoint").help("Endpoint path or absolute URL").required(true).num_args(1))
        .arg(Arg::new("header").long("header").short('H').help("Header 'Key: Value' (repeatable)").num_args(1).action(ArgAction::Append))
        .arg(Arg::new("body").long("body").help("Request body").num_args(1));
    app = app.subcommand(raw_cmd);

    // Add hierarchical commands
    app = add_children_commands(app, Vec::new(), &root);

    (app, leaf_map)
}

fn add_children_commands(mut app: Command, path: Vec<String>, node: &TreeNode) -> Command {
    // Add children of current node under the app
    for (name, child) in &node.children {
        let mut cmd = Command::new(leak_str(name.clone()));
        if let Some(about) = &child.about { cmd = cmd.about(leak_str(about.clone())); }
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
                        if let Some(def) = &arg.default { a = a.default_value(leak_str(def.clone())); }
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
                            if let Some(def) = &arg.default { a = a.default_value(leak_str(def.clone())); }
                        }

                        if let Some(l) = arg.long.as_deref() { a = a.long(leak_str(l.to_string())); } else if let Some(n) = arg.name.as_deref() { a = a.long(leak_str(n.to_string())); }
                        if let Some(s) = arg.short.as_deref() { a = a.short(s.chars().next().unwrap()); }
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
        if let Some(about) = &child.about { sub = sub.about(leak_str(about.clone())); }
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
                        if let Some(def) = &arg.default { a = a.default_value(leak_str(def.clone())); }
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
                            if let Some(def) = &arg.default { a = a.default_value(leak_str(def.clone())); }
                        }

                        if let Some(l) = arg.long.as_deref() { a = a.long(leak_str(l.to_string())); } else if let Some(n) = arg.name.as_deref() { a = a.long(leak_str(n.to_string())); }
                        if let Some(s) = arg.short.as_deref() { a = a.short(s.chars().next().unwrap()); }
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
        if name == "raw" { break; }
        path.push(name.to_string());
        current = sub_m;
        if sub_m.subcommand().is_none() { break; }
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
        let long = arg.long.clone().or_else(|| arg.name.clone()).unwrap_or_default();
        let help = arg.help.clone().unwrap_or_default();
        let required = arg.required.unwrap_or(false);
        println!("  --{} value  {}{}", long, help, if required { " (required)" } else { "" });
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

pub type CustomHandlerFn = dyn Fn(&HashMap<String, String>, &str, bool) -> anyhow::Result<()> + Send + Sync + 'static;

#[derive(Default)]
pub struct HandlerRegistry {
    handlers: HashMap<String, Box<CustomHandlerFn>>,
}

impl HandlerRegistry {
    #[must_use]
    pub fn new() -> Self { Self::default() }
    pub fn register<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&HashMap<String, String>, &str, bool) -> anyhow::Result<()> + Send + Sync + 'static,
    {
        self.handlers.insert(name.to_string(), Box::new(f));
    }
    pub fn get(&self, name: &str) -> Option<&CustomHandlerFn> { self.handlers.get(name).map(AsRef::as_ref) }
}

pub fn validate_handlers(root: &MappingRoot, registry: &HandlerRegistry) -> anyhow::Result<()> {
    let mut missing: Vec<String> = Vec::new();
    match root {
        MappingRoot::Flat(flat) => {
            for cmd in &flat.commands {
                if let Some(h) = &cmd.custom_handler {
                    if !registry.handlers.contains_key(h) { missing.push(h.clone()); }
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
                                if !reg.handlers.contains_key(h) { acc.push(h.clone()); }
                            }
                        }
                    }
                }
            }
            for g in &hier.commands { walk(g, registry, &mut missing); }
        }
    }
    if missing.is_empty() { Ok(()) } else { Err(anyhow::anyhow!("Missing custom handlers: {}", missing.join(", "))) }
}

fn parse_timeout(matches: &ArgMatches, arg_name: &str) -> Option<f64> {
    matches.get_one::<String>(arg_name).and_then(|s| s.parse::<f64>().ok()).filter(|v| *v >= 0.0)
}

fn collect_vars_from_matches(cmd: &CommandSpec, leaf: &ArgMatches) -> (HashMap<String, String>, HashSet<String>, bool) {
    let arg_specs: Vec<ArgSpec> = if cmd.args.is_empty() { derive_args_from_pattern(&cmd.pattern) } else { cmd.args.clone() };
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut selected: HashSet<String> = HashSet::new();
    let mut missing_required = false;
    for arg in &arg_specs {
        let name = arg.long.clone().or_else(|| arg.name.clone()).unwrap_or_default();
        if name.is_empty() { continue; }
        if arg.arg_type.as_deref() == Some("bool") {
            let is_set = leaf.get_flag(&name);
            if let Some(var_name) = arg.name.clone() {
                if is_set { selected.insert(var_name.clone()); }
                if let Some(cond) = &arg.value {
                    let val = match cond {
                        ConditionalValue::Mapping { if_set, if_not_set } => {
                            if is_set { if_set.clone() } else { if_not_set.clone() }
                        }
                        ConditionalValue::Sequence(entries) => {
                            if is_set { entries.iter().find_map(|e| e.if_set.clone()) } else { entries.iter().find_map(|e| e.if_not_set.clone()) }
                        }
                    }.unwrap_or_else(|| if is_set { "true".to_string() } else { "false".to_string() });
                    vars.insert(var_name, val);
                } else {
                    vars.insert(var_name, if is_set { "true".to_string() } else { "false".to_string() });
                }
            }
        } else if let Some(val) = leaf.get_one::<String>(&name) {
            if let Some(var_name) = arg.name.clone() { vars.insert(var_name.clone(), val.clone()); selected.insert(var_name); }
        } else if let Some(def) = &arg.default {
            if let Some(var_name) = arg.name.clone() { vars.insert(var_name, def.clone()); }
        } else if arg.required.unwrap_or(false) { missing_required = true; }
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
    let base_url = matches.get_one::<String>("base-url").cloned().unwrap_or_else(|| default_base_url.to_string());
    let json_output = matches.get_flag("json-output");
    let verbose = matches.get_flag("verbose");

    let config = ExecutionConfig {
        output: if json_output { OutputFormat::Json } else { OutputFormat::Human },
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
        let method = raw_m.get_one::<String>("method").cloned().unwrap_or_else(|| "GET".to_string());
        let endpoint = raw_m.get_one::<String>("endpoint").cloned().unwrap_or_default();
        let headers: Vec<String> = raw_m.get_many::<String>("header").map(|v| v.cloned().collect()).unwrap_or_default();
        let body = raw_m.get_one::<String>("body").cloned();
        let raw_spec = RawRequestSpec { base_url: Some(base_url.clone()), method, endpoint, headers, body, multipart: false, file_fields: HashMap::new(), table_view: None };
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
        if missing_required { print_manual_help(&path, cmd); return Ok(2); }
        let spec = build_request_from_command(Some(base_url.clone()), cmd, &vars, &selected);
        match &spec {
            RequestSpec::CustomHandler { handler_name, vars } => {
                let h = handlers.get(handler_name).ok_or_else(|| anyhow::anyhow!("No handler registered for {}", handler_name))?;
                h(vars, &base_url, json_output)?;
                Ok(0)
            }
            _ => execute_requests_loop(&spec, &config),
        }
    } else {
        // Intermediate path: print nested help
        let mut cmd = app2;
        for name in &path {
            let next_opt = cmd.get_subcommands().find(|c| c.get_name() == name).cloned();
            if let Some(next_cmd) = next_opt { cmd = next_cmd; } else { break; }
        }
        let _ = cmd.clone().print_help();
        println!();
        Ok(0)
    }
}
