# rclib - Build CLIs for REST API Servers (from YAML + OpenAPI)

The `rclib` name refers to Rust CLI builder or Rust CLI library

This library helps you build command-line interfaces for REST API servers. It turns a YAML mapping file and an OpenAPI specification into a dynamic CLI that can:
- Call API endpoints directly
- Orchestrate multi-step scenarios declaratively
- Delegate to custom imperative handlers implemented in Rust, when needed

## Core Concept

The library transforms YAML mapping files into fully functional CLI applications supporting three types of command implementations:

### 1. **Single API Call Commands**
Direct mapping from CLI arguments to a single HTTP request. The most common pattern for CRUD operations.

### 2. **Declarative Scenarios**
Multi-step operations defined in YAML with job scheduling, polling, and response extraction. Ideal for complex workflows that can be expressed declaratively.

### 3. **Custom Handler Commands**
Commands that require imperative logic implemented in the client application. Perfect for interactive operations, streaming responses, or complex business logic that cannot be expressed declaratively.

See Implemented Capabilities below for a concise list of features.

## Implemented Capabilities

- **Dynamic CLI from YAML**: Command tree, arguments, and help are generated from `mapping.yaml`.
- **Three command types**:
  - Single API calls (direct HTTP requests)
  - Declarative scenarios (multi-step with job scheduling + polling)
  - Custom handlers (imperative Rust code with validated variables)
- **Argument model**:
  - Inheritance and overrides (group/common args → command args)
  - Boolean flags with conditional values
  - File arguments that can override other variables (file content replacement)
- **Templating**: Substitute variables in endpoints, bodies, and headers (including built-ins like `{uuid}`).
- **HTTP features**:
  - Blocking client with `reqwest`
  - Headers and JSON bodies
  - Multipart file uploads
  - Base URL from OpenAPI `servers[0]` (overridable by `--base-url`)
- **Output**:
  - JSON mode (pretty printed)
  - Human mode with enhanced table view (column selection, nested paths, size modifiers)
- **Parallel execution & simple perf stats**:
  - `--count`, `--duration`, `--concurrency`
  - Prints success/error counts, average/min/max response time, and RPS
- **Runtime helpers**:
  - Handler registry + validation against `custom_handler:` in YAML
  - Utilities to rebuild the subcommand path and collect validated variables

## Architecture

Typical architecture: mapping.yaml + OpenAPI → rclib → CLI binary → REST API server:

```
OpenAPI Spec ── servers/schemas ─┐
                                 ├── rclib ──> CLI Binary(*) ──> REST API Server
mapping.yaml ── commands/args ───┘
```

> (*) The CLI Binary must be developed by the user following this guideline, and it can incorporate any additional custom logic as per the user's requirements.

### Key Components

- **OpenAPI Specification**: Provides API schema and endpoint definitions
- **mapping.yaml**: Defines the CLI command structure and REST API mappings
- **rclib**: Core library handling HTTP requests and argument processing
- **CLI Binary**: Command-line interface that uses the library

> Goal: keep API-specific knowledge declarative (YAML/OpenAPI), and allow custom code only where truly needed.

### Features

- **Flexible Arguments**: Supports positional args, flags, short flags, and default values
- **Boolean Flags**: Special handling for boolean arguments with conditional values
- **File Operations**: Support for file uploads and file content overrides
- **Multi-step Scenarios**: Complex operations that combine multiple HTTP requests
- **Custom Rust logic bindings**: Integrate custom imperative Rust logic into commands handlers
- **Repeated Execution**: Execute commands multiple times with configurable concurrency for performance testing
- **Performance Statistics**: Detailed timing and success/failure statistics for parallel executions

## mapping.yaml Structure

The mapping file uses a hierarchical structure to define CLI commands and their API mappings.

### Basic Structure

```yaml
common_args:                    # Top-level common arguments
  page:
    name: page
    help: "Page number"
    long: page
    required: false
    default: "1"

commands:                       # Command groups
  - name: command-group-name
    about: "Description of the command group"
    common_args:               # Group-level common arguments
      service:
        name: service
        help: "Service name"
        long: service
        required: true
    subcommands:
      - name: command-name
        about: "Command description"
        method: GET              # HTTP method
        endpoint: "/api/endpoint" # API endpoint
        args:                   # Command-specific arguments
          - inherit: service    # Inherit from common_args
          - name: id
            help: "Resource ID"
            long: id
            required: true
```

The example above would result in the following CLI options:

```
# Command path derived from name/subcommands
mycli command-group-name command-name \
  --service <service> \
  --id <id>

# Example invocation (with values)
mycli command-group-name command-name --service core --id 123

# Resulting HTTP request (templated by endpoint/body/headers)
GET /api/endpoint?service=core&id=123
```

### Argument Specification

Arguments support various types and behaviors:

```yaml
args:
  # Basic string argument
  - name: name
    help: "Resource name"
    long: name
    required: true

  # Positional argument
  - name: id
    help: "Resource ID"
    positional: true
    required: true

  # Boolean flag with conditional values
  - name: enabled
    help: "Enable feature"
    long: enabled
    type: bool
    value:
      if_set: "true"           # Value when flag is provided
      if_not_set: "false"      # Value when flag is not provided

  # File argument with content override
  - name: config_file
    help: "Path to config file"
    long: config-file
    type: file
    file-overrides-value-of: config  # Replaces 'config' variable with file content

  # Argument with default value
  - name: timeout
    help: "Request timeout"
    long: timeout
    default: "30"
```

### Inheritance and Overrides

Arguments can inherit from common definitions and override specific properties:

```yaml
common_args:
  service:
    name: service
    help: "Service name"
    long: service
    required: true

commands:
  - name: llm
    subcommands:
      - name: list-models
        method: GET
        endpoint: "/llm/models"
        args:
          - inherit: service
            endpoint: "/llm/services/{service}/models"  # Override endpoint
            required: false                             # Override required
```

Usage example:
```
# Without providing optional --service
mycli llm list-models
# → GET /llm/models

# With --service provided, endpoint override on the arg is applied
mycli llm list-models --service openai
# → GET /llm/services/openai/models
```

### File Upload Commands

Commands that handle file uploads use multipart encoding:

```yaml
- name: upload
  about: "Upload a file"
  method: POST
  endpoint: "/files/upload"
  multipart: true              # Enable multipart/form-data
  args:
    - name: file
      help: "File to upload"
      long: file
      required: true
      file_upload: true        # Mark as file upload field
```

### Scenario Commands

Multi-step operations that combine multiple HTTP requests:

```yaml
- name: model-install
  about: "Install a model with progress tracking"
  scenario:
    type: "job_with_polling"   # Built-in scenario type
    steps:
      - name: "schedule_job"
        method: POST
        endpoint: "/jobs"
        body: |
          {
            "type": "llm_model_ops:install",
            "idempotency_key": "{uuid}",    # Built-in variable
            "params": {
              "model_name": "{model_name}"  # User argument
            }
          }
        headers:
          Content-Type: application/json
        extract_response:
          job_id: "$.id"       # Extract job ID for next step

      - name: "poll_job"
        method: GET
        endpoint: "/jobs/{job_id}"         # Use extracted job_id
        polling:
          interval_seconds: 1
          timeout_seconds: 300
          completion_conditions:
            - status: "completed"
              action: "success"
            - status: "failed"
              action: "error"
              error_field: "$.error"
  args:
    - name: model_name
      help: "Model to install"
      long: model-name
      required: true
```

### Custom Handler Commands

Commands that require imperative logic implemented in the client application:

```yaml
- name: interactive
  about: "Start an interactive chat session"
  custom_handler: "chat_interactive"    # Custom handler name, must be implemented in Rust
  args:
    - name: thread_id
      help: "Chat thread ID to load history from"
      long: thread-id
      required: true
    - name: model
      help: "Model name"
      long: model
      default: "qwen2.5"
    - name: temperature
      help: "Temperature (0.0-2.0)"
      long: temperature
      default: "0.7"
    - name: hide_history
      help: "Don't show message history at start"
      long: hide-history
      type: bool
      value:
        if_set: "true"
        if_not_set: "false"
```

## Template Substitution

Templates use `{variable_name}` syntax and support:

- **URL paths**: `/api/users/{id}` → `/api/users/123`
- **Request bodies**: `{"name": "{name}"}` → `{"name": "example"}`
- **Headers**: `Authorization: Bearer {token}` → `Authorization: Bearer abc123`

Built-in variables:
- `{uuid}`: Auto-generated UUID for idempotency keys

## File Override Feature

The `file-overrides-value-of` feature allows reading file content to replace argument value:

```yaml
args:
  - name: params
    help: "JSON parameters"
    long: params
    default: "{}"

  - name: params_file
    help: "JSON file with parameters"
    long: params-file
    type: file
    file-overrides-value-of: params  # Replaces --params value with file content
```

Usage:
```bash
# Using inline parameters
cli command --params '{"key": "value"}'

# Using file (takes precedence)
cli command --params-file config.json
```

## Human-Readable Table Output

The library provides enhanced table formatting for array responses in human-readable mode. You can customize table columns and apply modifiers for better presentation.

### Table View Configuration

Use the `table_view` directive in command definitions to specify which columns to display:

```yaml
- name: list-files
  about: "List files with their sizes"
  method: GET
  endpoint: "/files"
  table_view: ["name", "size:gb", "type", "modified"]  # Custom columns with size modifier
  args:
    - name: directory
      help: "Directory to list"
      long: directory
      required: true
```

### Size Modifiers

Size modifiers convert byte values to human-readable units in table output. Supported modifiers:

- `:gb` or `:GB` - Convert bytes to gigabytes
- `:mb` or `:MB` - Convert bytes to megabytes
- `:kb` or `:KB` - Convert bytes to kilobytes

Example output:
```
+----------+------+----------+---------------------+
| Name     | Size | Type     | Modified            |
|          | GB   |          |                     |
+----------+------+----------+---------------------+
| data.db  | 2.34 | database | 2024-01-15 10:30:00 |
| logs.txt | 0.05 | text     | 2024-01-15 11:00:00 |
+----------+------+----------+---------------------+
```

### Table Formatting Features

- **Multi-line headers**: Column names are split by whitespace, each word on its own line
- **Auto-sizing**: Column widths adjust to content
- **Nested object flattening**: Automatically includes nested object properties (e.g., `capabilities.install_model`)
- **ASCII borders**: Clean table borders with `+`, `-`, and `|` characters

## Parallel Execution and Performance Testing

The library supports executing requests multiple times with configurable duration and concurrency for simple performance testing and load testing scenarios.

### Basic Usage

Execute a request multiple times:

```bash
# Execute the same request 100 times
mycli --count 100 api-command

# Execute with 10 concurrent requests
mycli --count 100 --concurrency 10 api-command

# Execute requests for 30 seconds
mycli --duration 30 api-command

# Execute requests for 60 seconds with 5 concurrent requests
mycli --duration 60 --concurrency 5 api-command
```

### Performance Statistics

The library supports simple performance testing:

- `-n, --count N`: Execute the command N times (default: 1)
- `-d, --duration N`: Execute the command for N seconds (overrides `--count`)
- `-c, --concurrency N`: Execute up to N commands in parallel (default: 1)

The library supports two execution modes:

1. **Count-based (`--count`)**: Execute a specific number of requests
   - Use when you need a precise number of requests for testing
   - Good for reproducible benchmarks
   - Example: `--count 1000`

2. **Duration-based (`--duration`)**: Execute requests for a specific time period
   - Use for time-bounded load testing
   - Good for sustained load testing scenarios
   - Overrides `--count` when specified
   - Example: `--duration 300` (5 minutes)

When using `--count` or `--duration`, the library automatically provides execution statistics:

```
======= Execution Summary =======
Concurrency:             10
Total execution time:    12.450s
Total requests:          100
Successful requests:     98 (98%)
Failed requests:         2 (2%)
Average response time:   0.124s  (min: 0.120s, max: 0.214s)
Requests per second:     8.03
```

### Use Cases

- **Load testing**: Test API performance under concurrent load using `--duration` for sustained testing
- **Reliability testing**: Execute the same request many times with `--count` to identify intermittent failures
- **Performance benchmarking**: Measure response times and throughput with precise `--count` control
- **Stress testing**: Test API behavior with high concurrency using `--concurrency`
- **Endurance testing**: Run requests for extended periods using `--duration`

## Library Usage

### Complete Main Function Example

Minimal `main()` using built-in driving, timeouts, and handler registry:

```rust
use std::env;
use std::fs;
use anyhow::{Context, Result};
use rclib::{self};

mod chat_helper; // your custom handlers live here

const EMBEDDED_OPENAPI: &str = include_str!("openapi.spec"); // example of static API spec injection
const EMBEDDED_MAPPING: &str = include_str!("mapping.yaml"); // example of static mapping injection
const APP_NAME: &str = "mycli";

fn main() {
    if let Err(err) = real_main() {
        eprintln!("Error: {:#}", err);
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    // Load OpenAPI and mapping (can be embedded or external files)
    let openapi_text = fs::read_to_string("openapi.yaml").unwrap_or_else(|_| EMBEDDED_OPENAPI.to_string());
    let openapi = rclib::parse_openapi(&openapi_text).context("OpenAPI parsing failed")?;
    let default_base_url = openapi.servers.get(0).map(|s| s.url.clone()).unwrap_or_else(|| "http://localhost:8080".to_string());

    let mapping_yaml = fs::read_to_string("mapping.yaml").unwrap_or_else(|_| EMBEDDED_MAPPING.to_string());
    let mapping_root = rclib::mapping::parse_mapping_root(&mapping_yaml)?;

    // Build CLI and parse args
    let (app, _) = rclib::cli::build_cli(&mapping_root, &default_base_url);
    let matches = app.get_matches();

    // Register custom handlers declared in mapping.yaml (custom_handler: "...")
    let mut registry = rclib::cli::HandlerRegistry::new();
    registry.register("chat_interactive", |vars, base_url, _json_output| {
        chat_helper::handle_chat_interactive(vars, base_url)?;
        Ok(())
    });

    // Ensure all declared handlers are provided
    rclib::cli::validate_handlers(&mapping_root, &registry)?;

    // Delegate execution to rclib (handles base-url/json-output/timeouts/raw/etc.)
    let user_agent = format!("{}/{}", APP_NAME, env!("CARGO_PKG_VERSION"));
    let exit_code = rclib::cli::drive_command(&mapping_root, &default_base_url, &matches, &registry, &user_agent)?;
    std::process::exit(exit_code);
}
```

### Runtime Orchestration (Registry and Execution)

The library provides a small runtime to minimize app code while keeping your logic pluggable:

- `HandlerRegistry` — register your custom handlers by name
  - `register(name, Fn(vars, base_url, json_output) -> Result<()>)`
- `validate_handlers(&MappingRoot, &HandlerRegistry)` — ensure all `custom_handler:` keys in mapping.yaml are registered
- `drive_command(&MappingRoot, default_base_url, &ArgMatches, &HandlerRegistry, user_agent)`
  - Handles built-in globals: `--base-url`, `--json-output`, `--conn-timeout`, `--timeout`
  - Supports `raw` requests and templated simple/scenario commands
  - Dispatches to custom handlers when `custom_handler` is present

### Adding Custom Global Options in main()

You can extend the CLI with your own global options and use them inside custom handlers. Example: add an optional `--org` flag and pass it into handlers.

```rust
use clap::Arg;

// Build CLI and add a custom global option
let (app, _) = rclib::cli::build_cli(&mapping_root, &default_base_url);
let app = app.arg(
    Arg::new("org")
        .long("org")
        .help("Organization ID to scope requests")
        .num_args(1)
);
let matches = app.get_matches();

// Read your custom option from matches
let org = matches.get_one::<String>("org").cloned();

// Register handlers and capture the option via closure
let mut registry = rclib::cli::HandlerRegistry::new();
let org_for_handler = org.clone();
registry.register("chat_interactive", move |vars, base_url, _json_output| {
    // Inject org (if provided) into the vars map seen by the handler
    let mut extended = vars.clone();
    if let Some(ref org_id) = org_for_handler { extended.insert("org".into(), org_id.clone()); }
    chat_helper::handle_chat_interactive(&extended, base_url)?;
    Ok(())
});

// Validate and drive as usual
rclib::cli::validate_handlers(&mapping_root, &registry)?;
let user_agent = format!("{}/{}", APP_NAME, env!("CARGO_PKG_VERSION"));
let exit_code = rclib::cli::drive_command(&mapping_root, &default_base_url, &matches, &registry, &user_agent)?;
std::process::exit(exit_code);
```

Notes:
- Custom globals are fully your responsibility (naming, validation, defaults). They are not interpreted by the library.
- Pass such options to handlers via captured variables (as shown) or your own shared state.

## Advanced Features

### Custom Handler Integration

Custom handlers act as a link between declarative YAML configuration and custom Rust logic. For example, an interactive chat feature is difficult to express purely in YAML, but you can still define its sub-command arguments declaratively in `mapping.yaml` while implementing the complex behavior in Rust:

```yaml
# Define arguments declaratively in mapping.yaml
- name: interactive-chat
  about: "Start interactive chat session"
  custom_handler: "chat_interactive" # refers to custom handler implemented in Rust
  args:
    - name: thread_id
      help: "Thread ID to load"
      long: thread-id
      required: true
    - name: model
      help: "Model to use"
      long: model
      default: "gpt-4"
```

The rclib library will:
1. Parse and validate all arguments according to the YAML definition
2. Generate appropriate help text and CLI structure
3. Pass validated arguments to your custom handler function
4. Let you implement any complex logic in Rust

Benefits:
- **Consistent UX**: All commands follow the same argument patterns
- **Automatic Validation**: Required arguments, types, and defaults are handled
- **Help Generation**: `--help` works automatically for custom commands
- **Maintainable**: Arguments are defined once, used everywhere

### Conditional Argument Overrides

Arguments can conditionally override request parameters:

```yaml
# Base command definition (list all models)
- name: list-models
  method: GET
  endpoint: "/llm/models"
  args:
    - name: service
      help: "Service name (optional for specific endpoint)"
      long: service
      required: false
      # When service is provided, override endpoint/method for this arg
      endpoint: "/llm/services/{service}/models"
      method: GET
```

Example:
```
# No service → use command-level endpoint
mycli llm list-models
# → GET /llm/models

# With service → use arg-level overridden endpoint
mycli llm list-models --service hf
# → GET /llm/services/hf/models
```

### Boolean Flag Handling

Boolean flags support conditional templating:

```yaml
args:
  - name: public
    help: "Make resource public"
    long: public
    type: bool
    value:
      if_set: "true"
      if_not_set: "false"

# In request body template:
body: '{"is_public": {public}}'  # Becomes true/false based on flag
```

### Error Handling

The library provides comprehensive error handling:

- **File Read Errors**: Graceful handling of missing or unreadable files
- **HTTP Errors**: Proper status code propagation
- **Template Errors**: Clear messages for missing variables
- **Validation Errors**: Argument validation and required field checking

## Best Practices

### Command Type Selection

1. **Use single API calls** for straightforward CRUD operations
2. **Use scenarios** for multi-step workflows that can be expressed declaratively
3. **Use custom handlers** for interactive operations, streaming, or complex business logic

### Argument Design

4. **Use inheritance** for common arguments across related commands
5. **Override conditionally** only when the argument significantly changes the request
6. **Make optional arguments** `required: false` when they provide alternative endpoints
7. **Use positional arguments** for primary resource identifiers
8. **Use flags** for optional parameters and filters
9. **Provide help text** for all arguments to improve usability

### Advanced Features

10. **Use file overrides** for complex JSON parameters that are better managed in files
11. **Define custom handler arguments in YAML** even when implementing imperative logic
12. **Keep custom handlers focused** on a single responsibility
13. **Pass structured data** to custom handlers via the validated argument map
14. **Handle errors gracefully** in custom handlers and return appropriate exit codes

## How This Compares To Other CLI Builders

- clap/structopt/argo (Rust argument parsers):
  - These are excellent for defining flags and subcommands in code
  - You still need to hand-write HTTP logic, URL templating, file overrides, and scenario orchestration
  - rclib complements them: it generates the CLI structure from YAML and wires variables into HTTP calls

- OpenAPI codegens (e.g., openapi-generator, oapi-codegen):
  - Generate strongly typed API clients, not end-user CLIs
  - You still write CLI layers, argument parsing, and command wiring
  - rclib focuses on the CLI layer, driven by YAML mapping and OpenAPI base URL, with templated endpoints/bodies

- Shell-based wrappers (curl + bash):
  - Quick to start, hard to scale/maintain; weak validation and help UX
  - rclib provides structured args, validation, consistent help, scenarios, and custom handler hooks

- Declarative CLIs without imperative hooks:
  - Great for simple calls; struggle with interactive streams or complex flows
  - rclib adds `custom_handler` so you can drop down to Rust when needed, while keeping args in YAML

When to pick rclib:
- You have a REST API and want a usable CLI quickly
- You prefer describing commands in YAML with templated endpoints/bodies
- You need both declarative and imperative paths (scenarios + custom handlers)

## Examples

See `dummyjson-cli` for comprehensive CLI tool example demonstrating all library features.
