//! Integration tests for rclib
//!
//! These tests verify that the various modules work together correctly.

use rclib::{
    build_request_from_command,
    mapping::{parse_mapping_root, MappingRoot},
    cli::{build_cli, collect_subcommand_path, collect_vars_from_matches, HandlerRegistry, validate_handlers},
    RequestSpec,
    OutputFormat,
    ExecutionConfig,
};

// ==================== Mapping → CLI Integration ====================

#[test]
fn test_full_cli_workflow_hierarchical() {
    // 1. Parse YAML mapping
    let yaml = r#"
commands:
  - name: users
    about: "User management commands"
    subcommands:
      - name: list
        about: "List all users"
        method: GET
        endpoint: /users
        args:
          - name: limit
            long: limit
            default: "10"
            help: "Maximum number of users"
      - name: get
        about: "Get a specific user"
        method: GET
        endpoint: /users/{id}
        args:
          - name: id
            positional: true
            required: true
            help: "User ID"
"#;
    let root = parse_mapping_root(yaml).unwrap();
    assert!(matches!(root, MappingRoot::Hier(_)));

    // 2. Build CLI from mapping
    let (app, path_map) = build_cli(&root, "https://api.example.com");

    // 3. Verify CLI structure
    assert!(path_map.contains_key(&vec!["users".to_string(), "list".to_string()]));
    assert!(path_map.contains_key(&vec!["users".to_string(), "get".to_string()]));

    // 4. Parse arguments
    let matches = app.try_get_matches_from(["cli", "users", "list", "--limit", "25"]).unwrap();
    let (path, leaf) = collect_subcommand_path(&matches);
    assert_eq!(path, vec!["users", "list"]);

    // 5. Get command and collect variables
    let cmd = path_map.get(&path).unwrap();
    let (vars, selected, missing) = collect_vars_from_matches(cmd, leaf);
    assert!(!missing);
    assert_eq!(vars.get("limit"), Some(&"25".to_string()));
    assert!(selected.contains("limit"));

    // 6. Build request spec
    let spec = build_request_from_command(Some("https://api.example.com".to_string()), cmd, &vars, &selected);
    if let RequestSpec::Simple(raw) = spec {
        assert_eq!(raw.method, "GET");
        assert_eq!(raw.endpoint, "/users");
    } else {
        panic!("Expected Simple request spec");
    }
}

#[test]
fn test_full_cli_workflow_with_path_params() {
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
"#;
    let root = parse_mapping_root(yaml).unwrap();
    let (app, path_map) = build_cli(&root, "https://api.example.com");

    let matches = app.try_get_matches_from(["cli", "users", "get", "123"]).unwrap();
    let (path, leaf) = collect_subcommand_path(&matches);
    let cmd = path_map.get(&path).unwrap();
    let (vars, selected, _) = collect_vars_from_matches(cmd, leaf);

    let spec = build_request_from_command(Some("https://api.example.com".to_string()), cmd, &vars, &selected);
    if let RequestSpec::Simple(raw) = spec {
        assert_eq!(raw.endpoint, "/users/123");
    } else {
        panic!("Expected Simple request spec");
    }
}

// ==================== Custom Handler Integration ====================

#[test]
fn test_custom_handler_workflow() {
    let yaml = r#"
commands:
  - name: export
    subcommands:
      - name: users
        custom_handler: export_users
        args:
          - name: format
            long: format
            default: "json"
          - name: output
            long: output
            default: "users.json"
"#;
    let root = parse_mapping_root(yaml).unwrap();

    // Validate handlers
    let mut reg = HandlerRegistry::new();
    reg.register("export_users", |vars, _base_url, _json| {
        // Verify we receive the expected variables
        assert!(vars.contains_key("format"));
        assert!(vars.contains_key("output"));
        Ok(())
    });
    assert!(validate_handlers(&root, &reg).is_ok());

    // Build and parse CLI
    let (app, path_map) = build_cli(&root, "https://api.example.com");
    let matches = app.try_get_matches_from(["cli", "export", "users", "--format", "csv"]).unwrap();
    let (path, leaf) = collect_subcommand_path(&matches);
    let cmd = path_map.get(&path).unwrap();
    let (vars, selected, _) = collect_vars_from_matches(cmd, leaf);

    // Build request spec
    let spec = build_request_from_command(None, cmd, &vars, &selected);
    if let RequestSpec::CustomHandler { handler_name, vars: handler_vars } = spec {
        assert_eq!(handler_name, "export_users");
        assert_eq!(handler_vars.get("format"), Some(&"csv".to_string()));
        assert_eq!(handler_vars.get("output"), Some(&"users.json".to_string()));
    } else {
        panic!("Expected CustomHandler request spec");
    }
}

// ==================== Template Substitution Integration ====================

#[test]
fn test_template_substitution_in_request() {
    let yaml = r#"
commands:
  - name: api
    subcommands:
      - name: call
        method: POST
        endpoint: /orgs/{org}/projects/{project}
        body: '{"name": "{name}", "description": "{desc}"}'
        headers:
          Authorization: "Bearer {token}"
          X-Org-Id: "{org}"
        args:
          - name: org
            positional: true
            required: true
          - name: project
            positional: true
            required: true
          - name: name
            long: name
            required: true
          - name: desc
            long: desc
            default: ""
          - name: token
            long: token
            required: true
"#;
    let root = parse_mapping_root(yaml).unwrap();
    let (app, path_map) = build_cli(&root, "https://api.example.com");

    let matches = app.try_get_matches_from([
        "cli", "api", "call", "acme", "myproject",
        "--name", "Test Project",
        "--desc", "A test project",
        "--token", "secret123"
    ]).unwrap();

    let (path, leaf) = collect_subcommand_path(&matches);
    let cmd = path_map.get(&path).unwrap();
    let (vars, selected, _) = collect_vars_from_matches(cmd, leaf);

    let spec = build_request_from_command(Some("https://api.example.com".to_string()), cmd, &vars, &selected);
    if let RequestSpec::Simple(raw) = spec {
        assert_eq!(raw.endpoint, "/orgs/acme/projects/myproject");
        assert_eq!(raw.body, Some(r#"{"name": "Test Project", "description": "A test project"}"#.to_string()));
        assert!(raw.headers.iter().any(|h| h.contains("Bearer secret123")));
        assert!(raw.headers.iter().any(|h| h.contains("X-Org-Id: acme")));
    } else {
        panic!("Expected Simple request spec");
    }
}

// ==================== Nested Groups Integration ====================

#[test]
fn test_deeply_nested_command_groups() {
    let yaml = r#"
commands:
  - name: cloud
    about: "Cloud commands"
    subcommands:
      - name: compute
        about: "Compute resources"
        subcommands:
          - name: instances
            about: "Instance management"
            subcommands:
              - name: list
                method: GET
                endpoint: /cloud/compute/instances
              - name: get
                method: GET
                endpoint: /cloud/compute/instances/{id}
                args:
                  - name: id
                    positional: true
                    required: true
"#;
    let root = parse_mapping_root(yaml).unwrap();
    let (app, path_map) = build_cli(&root, "https://api.example.com");

    // Verify deeply nested paths exist
    assert!(path_map.contains_key(&vec![
        "cloud".to_string(),
        "compute".to_string(),
        "instances".to_string(),
        "list".to_string()
    ]));

    // Parse and execute
    let matches = app.try_get_matches_from([
        "cli", "cloud", "compute", "instances", "get", "vm-123"
    ]).unwrap();

    let (path, leaf) = collect_subcommand_path(&matches);
    assert_eq!(path, vec!["cloud", "compute", "instances", "get"]);

    let cmd = path_map.get(&path).unwrap();
    let (vars, _, _) = collect_vars_from_matches(cmd, leaf);
    assert_eq!(vars.get("id"), Some(&"vm-123".to_string()));
}

// ==================== Boolean Flag Integration ====================

#[test]
fn test_boolean_flag_handling() {
    let yaml = r#"
commands:
  - name: users
    subcommands:
      - name: list
        method: GET
        endpoint: /users?verbose={verbose}
        args:
          - name: verbose
            long: verbose
            short: v
            type: bool
            value:
              if_set: "true"
              if_not_set: "false"
"#;
    let root = parse_mapping_root(yaml).unwrap();
    let (app, path_map) = build_cli(&root, "https://api.example.com");

    // Without flag
    let matches = app.clone().try_get_matches_from(["cli", "users", "list"]).unwrap();
    let (path, leaf) = collect_subcommand_path(&matches);
    let cmd = path_map.get(&path).unwrap();
    let (vars, _, _) = collect_vars_from_matches(cmd, leaf);
    assert_eq!(vars.get("verbose"), Some(&"false".to_string()));

    // With flag
    let matches = app.try_get_matches_from(["cli", "users", "list", "--verbose"]).unwrap();
    let (_path, leaf) = collect_subcommand_path(&matches);
    let (vars, _, _) = collect_vars_from_matches(cmd, leaf);
    assert_eq!(vars.get("verbose"), Some(&"true".to_string()));
}

// ==================== Common Args Integration ====================

#[test]
fn test_common_args_defined_in_group() {
    // Test common_args defined at group level, which is the supported pattern
    let yaml = r#"
commands:
  - name: users
    common_args:
      output_format:
        name: output_format
        long: output
        short: o
        default: "json"
        help: "Output format"
    subcommands:
      - name: list
        method: GET
        endpoint: /users
        use_common_args:
          - output_format
"#;
    let root = parse_mapping_root(yaml).unwrap();
    let (app, path_map) = build_cli(&root, "https://api.example.com");

    let matches = app.try_get_matches_from(["cli", "users", "list", "--output", "yaml"]).unwrap();
    let (path, leaf) = collect_subcommand_path(&matches);
    let cmd = path_map.get(&path).unwrap();
    let (vars, _, _) = collect_vars_from_matches(cmd, leaf);

    assert_eq!(vars.get("output_format"), Some(&"yaml".to_string()));
}

// ==================== ExecutionConfig Integration ====================

#[test]
fn test_execution_config_builder_pattern() {
    let config = ExecutionConfig {
        output: OutputFormat::Json,
        conn_timeout_secs: Some(30.0),
        request_timeout_secs: Some(60.0),
        user_agent: "test-cli/1.0",
        verbose: true,
        count: Some(10),
        duration_secs: 0,
        concurrency: 4,
    };

    assert_eq!(config.output, OutputFormat::Json);
    assert_eq!(config.conn_timeout_secs, Some(30.0));
    assert_eq!(config.concurrency, 4);
    assert!(config.verbose);
}

// ==================== Error Handling Integration ====================

#[test]
fn test_missing_required_handler() {
    let yaml = r#"
commands:
  - name: export
    subcommands:
      - name: data
        custom_handler: nonexistent_handler
"#;
    let root = parse_mapping_root(yaml).unwrap();
    let reg = HandlerRegistry::new();

    let result = validate_handlers(&root, &reg);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("nonexistent_handler"));
}

#[test]
fn test_invalid_yaml_mapping() {
    let invalid_yaml = "commands: [not: valid: yaml";
    let result = parse_mapping_root(invalid_yaml);
    assert!(result.is_err());
}

// ==================== Scenario Command Integration ====================

#[test]
fn test_scenario_command_parsing() {
    let yaml = r#"
commands:
  - name: deploy
    subcommands:
      - name: app
        about: "Deploy an application"
        scenario:
          type: sequential
          steps:
            - name: create_deployment
              method: POST
              endpoint: /deployments
              body: '{"app": "{app_name}"}'
              extract_response:
                deployment_id: "$.id"
            - name: check_status
              method: GET
              endpoint: /deployments/{deployment_id}
        args:
          - name: app_name
            long: app
            required: true
"#;
    let root = parse_mapping_root(yaml).unwrap();
    let (app, path_map) = build_cli(&root, "https://api.example.com");

    let matches = app.try_get_matches_from(["cli", "deploy", "app", "--app", "myapp"]).unwrap();
    let (path, leaf) = collect_subcommand_path(&matches);
    let cmd = path_map.get(&path).unwrap();
    let (vars, selected, _) = collect_vars_from_matches(cmd, leaf);

    let spec = build_request_from_command(Some("https://api.example.com".to_string()), cmd, &vars, &selected);
    if let RequestSpec::Scenario(scenario_spec) = spec {
        assert_eq!(scenario_spec.scenario.steps.len(), 2);
        assert_eq!(scenario_spec.scenario.steps[0].name, "create_deployment");
        assert!(scenario_spec.vars.contains_key("app_name"));
    } else {
        panic!("Expected Scenario request spec");
    }
}
