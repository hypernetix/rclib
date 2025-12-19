use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use jsonpath_lib as jsonpath;
use openapiv3::OpenAPI;
use regex::Regex;
use reqwest::blocking::{Client, ClientBuilder, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Method;
use serde_json::Value;
use uuid::Uuid;

pub mod cli;
pub mod mapping;

// =====================
// Public API
// =====================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Human,
    Quiet,
}

#[derive(Debug, Clone)]
pub struct RawRequestSpec {
    pub base_url: Option<String>,
    pub method: String,
    pub endpoint: String,
    pub headers: Vec<String>, // "Key: Value"
    pub body: Option<String>,
    pub multipart: bool,
    pub file_fields: HashMap<String, String>, // field_name -> file_path
    pub table_view: Option<Vec<String>>,      // optional column hints for array responses
}

#[derive(Debug, Clone)]
pub enum RequestSpec {
    Simple(RawRequestSpec),
    Scenario(ScenarioSpec),
    CustomHandler {
        handler_name: String,
        vars: HashMap<String, String>,
    },
}

#[derive(Debug, Clone)]
pub struct ScenarioSpec {
    pub base_url: Option<String>,
    pub scenario: mapping::Scenario,
    pub vars: HashMap<String, String>,
}

/// Configuration for request execution including timeouts, output format, and load testing options.
#[derive(Debug, Clone)]
pub struct ExecutionConfig<'a> {
    pub output: OutputFormat,
    pub conn_timeout_secs: Option<f64>,
    pub request_timeout_secs: Option<f64>,
    pub user_agent: &'a str,
    pub verbose: bool,
    pub count: Option<u32>,
    pub duration_secs: u32,
    pub concurrency: u32,
}

impl<'a> ExecutionConfig<'a> {
    #[must_use]
    pub fn new(user_agent: &'a str) -> Self {
        Self {
            output: OutputFormat::Human,
            conn_timeout_secs: None,
            request_timeout_secs: None,
            user_agent,
            verbose: false,
            count: None,
            duration_secs: 0,
            concurrency: 1,
        }
    }
}

/// Parse OpenAPI from YAML or JSON string.
pub fn parse_openapi(spec: &str) -> Result<OpenAPI> {
    // Try YAML first, then JSON
    let yaml_attempt = serde_yaml::from_str::<OpenAPI>(spec);
    if let Ok(api) = yaml_attempt {
        return Ok(api);
    }
    let json_attempt = serde_json::from_str::<OpenAPI>(spec)
        .context("Failed to parse OpenAPI as YAML, and also failed to parse as JSON")?;
    Ok(json_attempt)
}

/// Apply file overrides: for args with type="file" and file_overrides_value_of,
/// read file content and insert into vars under the target variable name.
fn apply_file_overrides(args: &[mapping::ArgSpec], vars: &mut HashMap<String, String>) {
    for arg in args {
        let dominated_var = arg.arg_type.as_deref() == Some("file")
            && arg.file_overrides_value_of.is_some()
            && arg
                .name
                .as_ref()
                .and_then(|n| vars.get(n))
                .is_some_and(|p| !p.is_empty());
        if !dominated_var {
            continue;
        }

        let target_var = arg.file_overrides_value_of.as_ref().unwrap();
        let file_path = vars.get(arg.name.as_ref().unwrap()).unwrap();

        if let Ok(content) = std::fs::read_to_string(file_path) {
            vars.insert(target_var.clone(), content);
        }
    }
}

/// Build a RequestSpec from a command entry and variable map, handling simple, scenario, and custom handler commands.
pub fn build_request_from_command(
    base_url: Option<String>,
    cmd: &mapping::CommandSpec,
    vars: &HashMap<String, String>,
    selected_args: &HashSet<String>,
) -> RequestSpec {
    // Check if this is a custom handler command
    if let Some(handler_name) = &cmd.custom_handler {
        // Add built-in variables
        let mut vars_with_builtins = vars.clone();
        vars_with_builtins.insert("uuid".to_string(), Uuid::new_v4().to_string());

        apply_file_overrides(&cmd.args, &mut vars_with_builtins);

        return RequestSpec::CustomHandler {
            handler_name: handler_name.clone(),
            vars: vars_with_builtins,
        };
    }

    // Check if this is a scenario command
    if let Some(scenario) = &cmd.scenario {
        // Add built-in variables
        let mut vars_with_builtins = vars.clone();
        vars_with_builtins.insert("uuid".to_string(), Uuid::new_v4().to_string());

        apply_file_overrides(&cmd.args, &mut vars_with_builtins);

        return RequestSpec::Scenario(ScenarioSpec {
            base_url,
            scenario: scenario.clone(),
            vars: vars_with_builtins,
        });
    }

    // Handle regular command
    let method = cmd
        .method
        .as_ref()
        .expect("method is required for non-scenario commands")
        .clone();
    let endpoint_template = cmd
        .endpoint
        .as_ref()
        .expect("endpoint is required for non-scenario commands");

    // Add built-in variables for regular commands too
    let mut vars_with_builtins = vars.clone();
    vars_with_builtins.insert("uuid".to_string(), Uuid::new_v4().to_string());

    apply_file_overrides(&cmd.args, &mut vars_with_builtins);

    // Start with command-level values
    let mut method = method;
    let mut endpoint = substitute_template(endpoint_template, &vars_with_builtins);
    let mut body = cmd
        .body
        .as_ref()
        .map(|t| substitute_template(t, &vars_with_builtins));
    let mut headers_map: HashMap<String, String> = cmd
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), substitute_template(v, &vars_with_builtins)))
        .collect();

    // Apply per-arg overrides, if present
    // If an ArgSpec has endpoint/method/headers/body filled, they override command values
    for a in &cmd.args {
        if let Some(arg_name) = a.name.as_ref() {
            if selected_args.contains(arg_name) {
                if let Some(ep) = &a.endpoint {
                    endpoint = substitute_template(ep, &vars_with_builtins);
                }
                if let Some(m) = &a.method {
                    method = m.clone();
                }
                if let Some(hdrs) = &a.headers {
                    for (k, v) in hdrs {
                        headers_map.insert(k.clone(), substitute_template(v, &vars_with_builtins));
                    }
                }
                if let Some(b) = &a.body {
                    body = Some(substitute_template(b, &vars_with_builtins));
                }
            }
        }
    }

    let headers: Vec<String> = headers_map
        .into_iter()
        .map(|(k, v)| format!("{}: {}", k, v))
        .collect();

    // Handle multipart uploads
    let mut file_fields: HashMap<String, String> = HashMap::new();
    if cmd.multipart {
        for arg in &cmd.args {
            if arg.file_upload {
                if let Some(arg_name) = &arg.name {
                    if let Some(file_path) = vars_with_builtins.get(arg_name) {
                        file_fields.insert(arg_name.clone(), file_path.clone());
                    }
                }
            }
        }
    }

    RequestSpec::Simple(RawRequestSpec {
        base_url,
        method,
        endpoint,
        headers,
        body,
        multipart: cmd.multipart,
        file_fields,
        table_view: cmd.table_view.clone(),
    })
}

/// Execute a request and print output according to format.
pub fn execute_request(
    spec: &RawRequestSpec,
    output: OutputFormat,
    user_agent: &str,
) -> Result<i32> {
    execute_request_with_timeout(spec, output, None, None, user_agent, &HashSet::new(), false)
}

/// Execute either a simple request or a scenario.
pub fn execute_request_spec(
    spec: &RequestSpec,
    output: OutputFormat,
    conn_timeout_secs: Option<f64>,
    request_timeout_secs: Option<f64>,
    user_agent: &str,
    verbose: bool,
) -> Result<i32> {
    match spec {
        RequestSpec::Simple(raw_spec) => execute_request_with_timeout(
            raw_spec,
            output,
            conn_timeout_secs,
            request_timeout_secs,
            user_agent,
            &HashSet::new(),
            verbose,
        ),
        RequestSpec::Scenario(scenario_spec) => execute_scenario(
            scenario_spec,
            output,
            conn_timeout_secs,
            request_timeout_secs,
            user_agent,
            verbose,
        ),
        RequestSpec::CustomHandler { .. } => {
            // Custom handlers should not reach this function - they are handled by the calling application
            bail!(
                "Custom handlers should be handled by the calling application, not by the library"
            );
        }
    }
}

#[derive(Debug, Clone)]
struct ExecutionResult {
    duration: Duration,
    is_success: bool,
}

/// Worker function that executes a single request
fn execute_worker_request(
    spec: &RequestSpec,
    output: OutputFormat,
    conn_timeout_secs: Option<f64>,
    request_timeout_secs: Option<f64>,
    user_agent: &str,
    _request_index: u32,
) -> ExecutionResult {
    let start = Instant::now();
    let result = execute_request_spec(
        spec,
        output,
        conn_timeout_secs,
        request_timeout_secs,
        user_agent,
        false, // Disable verbose for individual requests
    );
    let duration = start.elapsed();

    match result {
        Ok(exit_code) => {
            let is_success = exit_code == 0;
            ExecutionResult {
                duration,
                is_success,
            }
        }
        Err(_e) => ExecutionResult {
            duration,
            is_success: false,
        },
    }
}

/// Execute a request with count, duration, and concurrency control
pub fn execute_requests_loop(spec: &RequestSpec, config: &ExecutionConfig<'_>) -> Result<i32> {
    let ExecutionConfig {
        output,
        conn_timeout_secs,
        request_timeout_secs,
        user_agent,
        verbose,
        count,
        duration_secs,
        concurrency,
    } = *config;

    // Determine execution mode: duration-based or count-based
    let use_duration = duration_secs > 0;
    let target_count = if use_duration {
        None // No count limit when using duration
    } else {
        match count {
            Some(c) if c > 1 => Some(c),
            _ => {
                return execute_request_spec(
                    spec,
                    output,
                    conn_timeout_secs,
                    request_timeout_secs,
                    user_agent,
                    verbose,
                )
            }
        }
    };

    // Validate concurrency
    let concurrency = if concurrency == 0 { 1 } else { concurrency };

    // Custom handlers cannot be executed in parallel
    if matches!(spec, RequestSpec::CustomHandler { .. }) {
        if use_duration {
            eprintln!("Warning: Custom handlers cannot be executed with duration. Ignoring --duration option.");
        } else {
            eprintln!(
                "Warning: Custom handlers cannot be executed in parallel. Ignoring --count option."
            );
        }
        return execute_request_spec(
            spec,
            output,
            conn_timeout_secs,
            request_timeout_secs,
            user_agent,
            verbose,
        );
    }

    if verbose {
        if use_duration {
            eprintln!(
                "Executing requests for {} seconds with concurrency {}",
                duration_secs, concurrency
            );
        } else if let Some(count) = target_count {
            eprintln!(
                "Executing {} requests with concurrency {}",
                count, concurrency
            );
        }
    }

    let overall_start = Instant::now();
    let duration_limit = Duration::from_secs(duration_secs as u64);

    // Shared state using atomic counters
    let executed_count = Arc::new(AtomicU32::new(0));
    let should_stop = Arc::new(AtomicBool::new(false));

    // Channel for collecting results
    let (tx, rx) = mpsc::channel::<ExecutionResult>();

    // Create thread pool
    let mut handles = Vec::new();
    for _worker_id in 0..concurrency {
        let spec_clone = Arc::new(spec.clone());
        let user_agent_clone = user_agent.to_string();
        let executed_count_clone = Arc::clone(&executed_count);
        let should_stop_clone = Arc::clone(&should_stop);
        let tx_clone = tx.clone();

        let use_duration_clone = use_duration;
        let target_count_clone = target_count;
        let verbose_clone = verbose;
        let handle = thread::spawn(move || {
            loop {
                // Check stop conditions
                if use_duration_clone {
                    if should_stop_clone.load(Ordering::Relaxed) {
                        break;
                    }
                } else {
                    // Count mode: simple check before incrementing
                    if executed_count_clone.load(Ordering::Relaxed)
                        >= target_count_clone.unwrap_or(0)
                    {
                        break;
                    }
                }

                // Atomically get next request number
                let request_index = executed_count_clone.fetch_add(1, Ordering::SeqCst) + 1;

                // In count mode, check if we went over the limit
                if !use_duration_clone && request_index > target_count_clone.unwrap_or(0) {
                    break; // Don't execute this request
                }

                // Execute the request
                let worker_output = if verbose_clone {
                    OutputFormat::Json
                } else {
                    OutputFormat::Quiet
                };
                let result = execute_worker_request(
                    &spec_clone,
                    worker_output,
                    conn_timeout_secs,
                    request_timeout_secs,
                    &user_agent_clone,
                    request_index,
                );

                // Send result back
                if tx_clone.send(result).is_err() {
                    break; // Main thread has dropped the receiver
                }
            }
        });

        handles.push(handle);
    }

    // Drop the original sender so we can detect when all workers are done
    drop(tx);

    // Monitor duration for duration-based execution
    if use_duration {
        let should_stop_monitor = Arc::clone(&should_stop);
        let _monitor_handle = thread::spawn(move || {
            thread::sleep(duration_limit);
            should_stop_monitor.store(true, Ordering::Relaxed);
        });
    }

    // Collect results from workers
    let mut success_count = 0;
    let mut error_count = 0;
    let mut total_response_duration = Duration::from_millis(0);
    let mut min_response_time: Option<Duration> = None;
    let mut max_response_time: Option<Duration> = None;

    // Receive results until all workers are done
    while let Ok(result) = rx.recv() {
        total_response_duration += result.duration;

        // Update min/max response times
        min_response_time =
            Some(min_response_time.map_or(result.duration, |min| min.min(result.duration)));
        max_response_time =
            Some(max_response_time.map_or(result.duration, |max| max.max(result.duration)));

        if result.is_success {
            success_count += 1;
        } else {
            error_count += 1;
        }
    }

    // Wait for all worker threads to complete
    for handle in handles {
        let _ = handle.join();
    }

    let final_executed_count = executed_count.load(Ordering::Relaxed);
    let overall_duration = overall_start.elapsed();

    // Print summary
    if final_executed_count > 1 && !matches!(output, OutputFormat::Json) {
        println!("======= Execution Summary =======");
        println!("Concurrency:            {}", concurrency);
        println!(
            "Total execution time:   {:.3}s",
            overall_duration.as_secs_f64()
        );
        println!("Executed requests:      {}", final_executed_count);
        if error_count > 0 {
            println!(
                "Successful requests:    {} ({:.0}%)",
                success_count,
                (success_count as f64 / final_executed_count as f64) * 100.0
            );
            println!(
                "Failed requests:        {} ({:.0}%)",
                error_count,
                (error_count as f64 / final_executed_count as f64) * 100.0
            );
        } else {
            println!("Successful requests:    {}", success_count);
            println!("Failed requests:        {}", error_count);
        }
        if final_executed_count > 0 {
            println!(
                "Average response time:  {:.3}s  (min: {:.3}s, max: {:.3}s)",
                total_response_duration.as_secs_f64() / final_executed_count as f64,
                min_response_time
                    .unwrap_or(Duration::from_millis(0))
                    .as_secs_f64(),
                max_response_time
                    .unwrap_or(Duration::from_millis(0))
                    .as_secs_f64()
            );
            println!(
                "Requests per second:    {:.2}",
                final_executed_count as f64 / overall_duration.as_secs_f64()
            );
        }
    }

    // Return appropriate exit code
    if error_count > 0 {
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Execute a request with optional connection and request timeout seconds.
pub fn execute_request_with_timeout(
    spec: &RawRequestSpec,
    output: OutputFormat,
    conn_timeout_secs: Option<f64>,
    request_timeout_secs: Option<f64>,
    user_agent: &str,
    _selected_args: &HashSet<String>,
    verbose: bool,
) -> Result<i32> {
    let mut builder: ClientBuilder = Client::builder().user_agent(user_agent);
    if let Some(secs) = conn_timeout_secs {
        builder = builder.connect_timeout(std::time::Duration::from_secs_f64(secs));
    }
    if let Some(secs) = request_timeout_secs {
        builder = builder.timeout(std::time::Duration::from_secs_f64(secs));
    }
    let client = builder.build().context("Failed to build HTTP client")?;

    let url = build_url(&spec.base_url, &spec.endpoint)?;
    let method = parse_method(&spec.method)?;
    let mut req = client.request(method, url);

    // Headers (don't set Content-Type for multipart - reqwest will set it)
    let mut extra_headers = parse_headers(&spec.headers)?;
    if spec.multipart {
        extra_headers.remove("content-type");
        extra_headers.remove("Content-Type");
    }
    if !extra_headers.is_empty() {
        req = req.headers(extra_headers);
    }

    if spec.multipart && !spec.file_fields.is_empty() {
        // Build multipart form
        let mut form = reqwest::blocking::multipart::Form::new();

        for (field_name, file_path) in &spec.file_fields {
            let file_contents = std::fs::read(file_path)
                .with_context(|| format!("Failed to read file: {}", file_path))?;
            let file_name = std::path::Path::new(file_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file");

            let part = reqwest::blocking::multipart::Part::bytes(file_contents)
                .file_name(file_name.to_string());
            form = form.part(field_name.clone(), part);
        }

        req = req.multipart(form);
    } else if let Some(body) = &spec.body {
        req = req.body(body.clone());
    }

    let full_url = build_url(&spec.base_url, &spec.endpoint)?;
    if verbose {
        eprintln!("-> {} {}", spec.method, full_url);
        if !spec.headers.is_empty() {
            eprintln!("-> Headers:");
            for h in &spec.headers {
                eprintln!("   {}", h);
            }
        }
        if let Some(b) = &spec.body {
            eprintln!("-> Body: {}", b);
        }
    }

    let started = std::time::Instant::now();
    let resp = req.send().context("HTTP request failed")?;
    let elapsed_ms = started.elapsed().as_millis();
    if verbose {
        eprintln!(
            "<- {} {} ({} ms)",
            resp.status().as_u16(),
            full_url,
            elapsed_ms
        );
    }
    output_response(resp, output, spec.table_view.as_ref())
}

/// Execute a scenario with multiple steps.
pub fn execute_scenario(
    scenario_spec: &ScenarioSpec,
    output: OutputFormat,
    conn_timeout_secs: Option<f64>,
    request_timeout_secs: Option<f64>,
    user_agent: &str,
    verbose: bool,
) -> Result<i32> {
    let mut variables = scenario_spec.vars.clone();

    match scenario_spec.scenario.scenario_type.as_str() {
        "job_with_polling" => execute_job_with_polling_scenario(
            scenario_spec,
            &mut variables,
            output,
            conn_timeout_secs,
            request_timeout_secs,
            user_agent,
            verbose,
        ),
        _ => {
            bail!(
                "Unsupported scenario type: {}",
                scenario_spec.scenario.scenario_type
            )
        }
    }
}

/// Execute a job_with_polling scenario.
fn execute_job_with_polling_scenario(
    scenario_spec: &ScenarioSpec,
    variables: &mut HashMap<String, String>,
    output: OutputFormat,
    conn_timeout_secs: Option<f64>,
    request_timeout_secs: Option<f64>,
    user_agent: &str,
    verbose: bool,
) -> Result<i32> {
    if scenario_spec.scenario.steps.len() != 2 {
        bail!("job_with_polling scenario must have exactly 2 steps (schedule_job, poll_job)");
    }

    // Step 1: Schedule job
    let schedule_step = &scenario_spec.scenario.steps[0];
    if schedule_step.name != "schedule_job" {
        bail!("First step must be named 'schedule_job'");
    }

    let schedule_spec =
        build_raw_spec_from_step(&scenario_spec.base_url, schedule_step, variables)?;
    if verbose {
        eprintln!(
            "-> {} {}",
            schedule_spec.method,
            build_url(&schedule_spec.base_url, &schedule_spec.endpoint)?
        );
    }
    let schedule_response = execute_single_request(
        &schedule_spec,
        conn_timeout_secs,
        request_timeout_secs,
        user_agent,
    )?;

    // Extract response variables
    extract_response_variables(
        &schedule_response,
        &schedule_step.extract_response,
        variables,
    )?;

    if output == OutputFormat::Json {
        println!("Step 1 (schedule_job) completed");
    } else {
        let job_id = variables
            .get("job_id")
            .map(|s| s.as_str())
            .unwrap_or("unknown");
        println!("Job scheduled with ID: {}", job_id);
        println!("Waiting for job to complete...");
    }

    // Step 2: Poll job
    let poll_step = &scenario_spec.scenario.steps[1];
    if poll_step.name != "poll_job" {
        bail!("Second step must be named 'poll_job'");
    }

    let polling_config = poll_step
        .polling
        .as_ref()
        .context("poll_job step must have polling configuration")?;

    let start_time = Instant::now();
    let timeout_duration = Duration::from_secs(polling_config.timeout_seconds);

    loop {
        if start_time.elapsed() > timeout_duration {
            bail!(
                "Polling timeout after {} seconds",
                polling_config.timeout_seconds
            );
        }

        let poll_spec = build_raw_spec_from_step(&scenario_spec.base_url, poll_step, variables)?;
        if verbose {
            eprintln!(
                "-> {} {}",
                poll_spec.method,
                build_url(&poll_spec.base_url, &poll_spec.endpoint)?
            );
        }
        let poll_response = execute_single_request(
            &poll_spec,
            conn_timeout_secs,
            request_timeout_secs,
            user_agent,
        )?;

        // Parse response to check completion condition
        let response_json: Value = serde_json::from_str(&poll_response)
            .context("Failed to parse polling response as JSON")?;

        // Check completion conditions
        for condition in &polling_config.completion_conditions {
            if let Some(status_value) = response_json.get("status") {
                if let Some(status_str) = status_value.as_str() {
                    if status_str == condition.status {
                        match condition.action.as_str() {
                            "success" => {
                                if output == OutputFormat::Json {
                                    println!("{}", poll_response);
                                } else {
                                    println!("Operation completed successfully");
                                }
                                return Ok(0);
                            }
                            "error" => {
                                let error_msg = if let Some(error_field) = &condition.error_field {
                                    extract_jsonpath_value(&response_json, error_field)
                                        .unwrap_or_else(|| "Unknown error".to_string())
                                } else if let Some(error_msg) = &condition.error_message {
                                    error_msg.clone()
                                } else {
                                    "Operation failed".to_string()
                                };

                                if output == OutputFormat::Json {
                                    println!("{}", poll_response);
                                } else {
                                    eprintln!("Error: {}", error_msg);
                                }
                                return Ok(1);
                            }
                            _ => {
                                bail!("Unknown completion action: {}", condition.action);
                            }
                        }
                    }
                }
            }
        }

        // Show progress if available and not in JSON mode
        if output != OutputFormat::Json {
            if let Some(progress_value) = response_json.get("progress") {
                if let Some(progress) = progress_value.as_f64() {
                    print!("\rProgress: {:.1}%", progress);
                    std::io::Write::flush(&mut std::io::stdout()).ok();
                }
            }
        }

        // Wait before next poll
        std::thread::sleep(Duration::from_secs(polling_config.interval_seconds));
    }
}

/// Build a RawRequestSpec from a scenario step.
fn build_raw_spec_from_step(
    base_url: &Option<String>,
    step: &mapping::ScenarioStep,
    variables: &HashMap<String, String>,
) -> Result<RawRequestSpec> {
    let endpoint = substitute_template(&step.endpoint, variables);
    let body = step
        .body
        .as_ref()
        .map(|b| substitute_template(b, variables));
    let headers: Vec<String> = step
        .headers
        .iter()
        .map(|(k, v)| format!("{}: {}", k, substitute_template(v, variables)))
        .collect();

    Ok(RawRequestSpec {
        base_url: base_url.clone(),
        method: step.method.clone(),
        endpoint,
        headers,
        body,
        multipart: false, // Scenarios don't currently support multipart
        file_fields: HashMap::new(),
        table_view: None,
    })
}

/// Execute a single HTTP request and return the response body as a string.
fn execute_single_request(
    spec: &RawRequestSpec,
    conn_timeout_secs: Option<f64>,
    request_timeout_secs: Option<f64>,
    user_agent: &str,
) -> Result<String> {
    let mut builder: ClientBuilder = Client::builder().user_agent(user_agent);
    if let Some(secs) = conn_timeout_secs {
        builder = builder.connect_timeout(std::time::Duration::from_secs_f64(secs));
    }
    if let Some(secs) = request_timeout_secs {
        builder = builder.timeout(std::time::Duration::from_secs_f64(secs));
    }
    let client = builder.build().context("Failed to build HTTP client")?;

    let url = build_url(&spec.base_url, &spec.endpoint)?;
    let method = parse_method(&spec.method)?;
    let mut req = client.request(method, url);

    // Headers
    let extra_headers = parse_headers(&spec.headers)?;
    if !extra_headers.is_empty() {
        req = req.headers(extra_headers);
    }

    if let Some(body) = &spec.body {
        req = req.body(body.clone());
    }

    let resp = req.send().context("HTTP request failed")?;
    let status = resp.status();
    let body = resp.text().context("Failed to read response body")?;

    if !status.is_success() {
        bail!("HTTP request failed with status {}: {}", status, body);
    }

    Ok(body)
}

/// Extract variables from response using JSONPath expressions.
fn extract_response_variables(
    response_body: &str,
    extractions: &HashMap<String, String>,
    variables: &mut HashMap<String, String>,
) -> Result<()> {
    if extractions.is_empty() {
        return Ok(());
    }

    let response_json: Value = serde_json::from_str(response_body)
        .context("Failed to parse response as JSON for variable extraction")?;

    for (var_name, jsonpath_expr) in extractions {
        if let Some(value) = extract_jsonpath_value(&response_json, jsonpath_expr) {
            variables.insert(var_name.clone(), value);
        } else {
            bail!(
                "Failed to extract variable '{}' using JSONPath '{}'",
                var_name,
                jsonpath_expr
            );
        }
    }

    Ok(())
}

/// Extract a value from JSON using JSONPath expression.
fn extract_jsonpath_value(json: &Value, path: &str) -> Option<String> {
    use jsonpath::select;

    let results = select(json, path).ok()?;
    if let Some(first_result) = results.first() {
        match first_result {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => Some(first_result.to_string()),
        }
    } else {
        None
    }
}

// =====================
// Internal helpers
// =====================

fn build_url(base_url: &Option<String>, endpoint: &str) -> Result<String> {
    // Absolute endpoint
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return Ok(endpoint.to_string());
    }
    let base = base_url
        .as_ref()
        .context("Endpoint is relative but no --base-url provided")?;
    let has_slash = base.ends_with('/') || endpoint.starts_with('/');
    Ok(if has_slash {
        format!("{}{}", base.trim_end_matches('/'), endpoint)
    } else {
        format!("{}/{}", base, endpoint)
    })
}

fn parse_method(method: &str) -> Result<Method> {
    let m = match method.to_uppercase().as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        "HEAD" => Method::HEAD,
        "OPTIONS" => Method::OPTIONS,
        other => bail!("Unsupported HTTP method: {}", other),
    };
    Ok(m)
}

fn parse_headers(raw_headers: &[String]) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for h in raw_headers {
        let parts: Vec<&str> = h.splitn(2, ':').collect();
        if parts.len() != 2 {
            bail!("Invalid header format, expected 'Key: Value', got: {}", h);
        }
        let name = parts[0].trim();
        let value = parts[1].trim();
        let name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("Invalid header name: {}", name))?;
        let value = HeaderValue::from_str(value)
            .with_context(|| format!("Invalid header value for {}", name))?;
        map.insert(name, value);
    }
    Ok(map)
}

fn output_response(
    resp: Response,
    output: OutputFormat,
    table_view: Option<&Vec<String>>,
) -> Result<i32> {
    let status = resp.status();
    let text = resp.text().unwrap_or_default();

    match output {
        OutputFormat::Json => {
            let json_val: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| {
                serde_json::json!({
                    "status": status.as_u16(),
                    "body": text,
                })
            });
            println!("{}", serde_json::to_string_pretty(&json_val)?);
        }
        OutputFormat::Human => {
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&text) {
                print_human_readable(&json_val, table_view);
            } else {
                println!("{}", text);
            }
        }
        OutputFormat::Quiet => {
            // Do nothing
        }
    }

    if status.is_success() {
        Ok(0)
    } else {
        Ok(1)
    }
}

fn print_human_readable(v: &serde_json::Value, table_view: Option<&Vec<String>>) {
    match v {
        serde_json::Value::Object(map) => {
            // First pass: print scalar fields aligned
            let mut scalar_entries: Vec<(&String, &serde_json::Value)> = Vec::new();
            let mut array_entries: Vec<(&String, &serde_json::Value)> = Vec::new();
            for (k, val) in map.iter() {
                match val {
                    serde_json::Value::Array(_) => array_entries.push((k, val)),
                    _ => scalar_entries.push((k, val)),
                }
            }
            scalar_entries.sort_by_key(|(k, _)| *k);
            let width = scalar_entries
                .iter()
                .map(|(k, _)| k.len())
                .max()
                .unwrap_or(0);
            for (k, val) in scalar_entries {
                let s = scalar_to_string(val);
                println!("{key:width$}: {val}", key = k, width = width, val = s);
            }
            // Then print arrays as tables
            for (k, val) in array_entries {
                println!();
                println!("{}:", k);
                if let serde_json::Value::Array(arr) = val {
                    print_array_table(arr, table_view);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            print_array_table(arr, table_view);
        }
        _ => {
            println!("{}", scalar_to_string(v));
        }
    }
}

fn print_array_table(arr: &Vec<serde_json::Value>, table_view: Option<&Vec<String>>) {
    if arr.is_empty() {
        println!("(empty)");
        return;
    }

    // Parse column specifications (path and optional modifier)
    let col_specs: Vec<ColumnSpec> = if let Some(cols) = table_view {
        cols.iter().map(|c| parse_column_spec(c)).collect()
    } else {
        let mut derived: Vec<String> = Vec::new();
        for item in arr {
            if let serde_json::Value::Object(map) = item {
                for (k, v) in map.iter() {
                    match v {
                        serde_json::Value::Object(inner) => {
                            for inner_k in inner.keys() {
                                let path = format!("{}.{}", k, inner_k);
                                if !derived.contains(&path) {
                                    derived.push(path);
                                }
                            }
                        }
                        _ => {
                            if !derived.contains(k) {
                                derived.push(k.clone());
                            }
                        }
                    }
                }
            }
        }
        derived.iter().map(|c| parse_column_spec(c)).collect()
    };
    if col_specs.is_empty() {
        for (i, item) in arr.iter().enumerate() {
            println!("{:<6} {}", i, scalar_to_string(item));
        }
        return;
    }

    // Build header labels (humanized, multi-line by whitespace)
    let header_labels: Vec<String> = col_specs
        .iter()
        .map(|c| humanize_column_label_with_modifier(&c.path, &c.modifier))
        .collect();
    let header_lines: Vec<Vec<String>> = header_labels
        .iter()
        .map(|lbl| lbl.split_whitespace().map(|s| s.to_string()).collect())
        .collect();
    let header_max_lines = header_lines.iter().map(|v| v.len()).max().unwrap_or(1);

    // Compute column widths from header parts and cell data
    let mut widths: Vec<usize> = header_lines
        .iter()
        .map(|parts| parts.iter().map(|s| s.len()).max().unwrap_or(0))
        .collect();
    for item in arr {
        if let serde_json::Value::Object(_) = item {
            for (idx, col_spec) in col_specs.iter().enumerate() {
                let cell_val = get_value_by_path(item, &col_spec.path);
                let cell = scalar_to_string_with_modifier(cell_val, &col_spec.modifier);
                if cell.len() > widths[idx] {
                    widths[idx] = cell.len();
                }
            }
        }
    }

    // Separator line like +-----+-----+
    let mut sep_line = String::from("+");
    for w in &widths {
        sep_line.push_str(&"-".repeat(w + 2));
        sep_line.push('+');
    }
    println!("{}", sep_line);

    // Print header (multi-line)
    for line_idx in 0..header_max_lines {
        let mut parts: Vec<String> = Vec::new();
        for (i, col_parts) in header_lines.iter().enumerate() {
            let s = if line_idx < col_parts.len() {
                &col_parts[line_idx]
            } else {
                ""
            };
            parts.push(format!(" {:<width$} ", s, width = widths[i]));
        }
        println!("|{}|", parts.join("|"));
    }

    println!("{}", sep_line);

    // Rows
    for item in arr {
        if let serde_json::Value::Object(_) = item {
            let mut row_parts: Vec<String> = Vec::new();
            for (i, col_spec) in col_specs.iter().enumerate() {
                let cell_val = get_value_by_path(item, &col_spec.path);
                let cell = scalar_to_string_with_modifier(cell_val, &col_spec.modifier);
                row_parts.push(format!(" {:<width$} ", cell, width = widths[i]));
            }
            println!("|{}|", row_parts.join("|"));
        }
    }

    println!("{}", sep_line);
}

#[derive(Debug, Clone)]
struct ColumnSpec {
    path: String,
    modifier: Option<SizeModifier>,
}

#[derive(Debug, Clone)]
enum SizeModifier {
    Gigabytes,
    Megabytes,
    Kilobytes,
}

fn parse_column_spec(spec: &str) -> ColumnSpec {
    if let Some(colon_pos) = spec.find(':') {
        let path = spec[..colon_pos].to_string();
        let modifier_str = spec[colon_pos + 1..].to_lowercase();
        let modifier = match modifier_str.as_str() {
            "gb" => Some(SizeModifier::Gigabytes),
            "mb" => Some(SizeModifier::Megabytes),
            "kb" => Some(SizeModifier::Kilobytes),
            _ => None,
        };
        ColumnSpec { path, modifier }
    } else {
        ColumnSpec {
            path: spec.to_string(),
            modifier: None,
        }
    }
}

fn get_value_by_path<'a>(v: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    let mut current = v;
    for seg in path.split('.') {
        match current {
            serde_json::Value::Object(map) => {
                if let Some(next) = map.get(seg) {
                    current = next;
                } else {
                    return &serde_json::Value::Null;
                }
            }
            _ => return &serde_json::Value::Null,
        }
    }
    current
}

fn humanize_column_label(path: &str) -> String {
    let last = path.split('.').next_back().unwrap_or(path);
    let spaced = last.replace(['_', '-'], " ");
    let mut out_words: Vec<String> = Vec::new();
    for w in spaced.split_whitespace() {
        if w.is_empty() {
            continue;
        }
        let mut chars = w.chars();
        if let Some(first) = chars.next() {
            let mut s = String::new();
            s.push(first.to_ascii_uppercase());
            for c in chars {
                s.push(c.to_ascii_lowercase());
            }
            out_words.push(s);
        }
    }
    if out_words.is_empty() {
        last.to_string()
    } else {
        out_words.join(" ")
    }
}

fn humanize_column_label_with_modifier(path: &str, modifier: &Option<SizeModifier>) -> String {
    let base_label = humanize_column_label(path);
    match modifier {
        Some(SizeModifier::Gigabytes) => format!("{}\nGB", base_label),
        Some(SizeModifier::Megabytes) => format!("{}\nMB", base_label),
        Some(SizeModifier::Kilobytes) => format!("{}\nKB", base_label),
        None => base_label,
    }
}

fn scalar_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn scalar_to_string_with_modifier(
    v: &serde_json::Value,
    modifier: &Option<SizeModifier>,
) -> String {
    match modifier {
        Some(size_mod) => {
            // Try to parse as number for size conversion
            match v {
                serde_json::Value::Number(n) => {
                    if let Some(bytes) = n.as_f64() {
                        let converted = match size_mod {
                            SizeModifier::Gigabytes => bytes / (1024.0 * 1024.0 * 1024.0),
                            SizeModifier::Megabytes => bytes / (1024.0 * 1024.0),
                            SizeModifier::Kilobytes => bytes / 1024.0,
                        };
                        format!("{:.2}", converted)
                    } else {
                        scalar_to_string(v)
                    }
                }
                serde_json::Value::String(s) => {
                    // Try to parse string as number
                    if let Ok(bytes) = s.parse::<f64>() {
                        let converted = match size_mod {
                            SizeModifier::Gigabytes => bytes / (1024.0 * 1024.0 * 1024.0),
                            SizeModifier::Megabytes => bytes / (1024.0 * 1024.0),
                            SizeModifier::Kilobytes => bytes / 1024.0,
                        };
                        format!("{:.2}", converted)
                    } else {
                        s.clone()
                    }
                }
                _ => scalar_to_string(v),
            }
        }
        None => scalar_to_string(v),
    }
}

pub fn substitute_template(template: &str, vars: &HashMap<String, String>) -> String {
    // Replace {name} occurrences. Use a regex to find placeholders.
    static PLACEHOLDER_RE: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
        Regex::new(r"\{([a-zA-Z_][a-zA-Z0-9_]*)\}").expect("valid regex")
    });
    PLACEHOLDER_RE
        .replace_all(template, |caps: &regex::Captures| {
            let key = &caps[1];
            vars.get(key).cloned().unwrap_or_default()
        })
        .to_string()
}

// Re-export useful types for consumers
pub use openapiv3;
pub use reqwest;

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== substitute_template tests ====================

    #[test]
    fn test_substitute_template_single_var() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "John".to_string());
        let result = substitute_template("Hello {name}!", &vars);
        assert_eq!(result, "Hello John!");
    }

    #[test]
    fn test_substitute_template_multiple_vars() {
        let mut vars = HashMap::new();
        vars.insert("first".to_string(), "John".to_string());
        vars.insert("last".to_string(), "Doe".to_string());
        let result = substitute_template("{first} {last}", &vars);
        assert_eq!(result, "John Doe");
    }

    #[test]
    fn test_substitute_template_missing_var() {
        let vars = HashMap::new();
        let result = substitute_template("Hello {name}!", &vars);
        assert_eq!(result, "Hello !"); // Missing vars become empty
    }

    #[test]
    fn test_substitute_template_no_placeholders() {
        let vars = HashMap::new();
        let result = substitute_template("Hello World!", &vars);
        assert_eq!(result, "Hello World!");
    }

    #[test]
    fn test_substitute_template_empty_template() {
        let vars = HashMap::new();
        let result = substitute_template("", &vars);
        assert_eq!(result, "");
    }

    #[test]
    fn test_substitute_template_repeated_var() {
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "A".to_string());
        let result = substitute_template("{x}{x}{x}", &vars);
        assert_eq!(result, "AAA");
    }

    #[test]
    fn test_substitute_template_url_path() {
        let mut vars = HashMap::new();
        vars.insert("org".to_string(), "acme".to_string());
        vars.insert("id".to_string(), "123".to_string());
        let result = substitute_template("/orgs/{org}/users/{id}", &vars);
        assert_eq!(result, "/orgs/acme/users/123");
    }

    #[test]
    fn test_substitute_template_json_body() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Test".to_string());
        vars.insert("value".to_string(), "42".to_string());
        let template = r#"{"name": "{name}", "value": {value}}"#;
        let result = substitute_template(template, &vars);
        assert_eq!(result, r#"{"name": "Test", "value": 42}"#);
    }

    // ==================== ExecutionConfig tests ====================

    #[test]
    fn test_execution_config_new_defaults() {
        let config = ExecutionConfig::new("test-agent/1.0");
        assert_eq!(config.output, OutputFormat::Human);
        assert_eq!(config.conn_timeout_secs, None);
        assert_eq!(config.request_timeout_secs, None);
        assert_eq!(config.user_agent, "test-agent/1.0");
        assert!(!config.verbose);
        assert_eq!(config.count, None);
        assert_eq!(config.duration_secs, 0);
        assert_eq!(config.concurrency, 1);
    }

    // ==================== OutputFormat tests ====================

    #[test]
    fn test_output_format_equality() {
        assert_eq!(OutputFormat::Json, OutputFormat::Json);
        assert_eq!(OutputFormat::Human, OutputFormat::Human);
        assert_eq!(OutputFormat::Quiet, OutputFormat::Quiet);
        assert_ne!(OutputFormat::Json, OutputFormat::Human);
    }

    // ==================== build_request_from_command tests ====================

    #[test]
    fn test_build_request_simple_get() {
        let cmd = mapping::CommandSpec {
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
        let vars = HashMap::new();
        let selected = HashSet::new();
        let spec = build_request_from_command(
            Some("https://api.example.com".to_string()),
            &cmd,
            &vars,
            &selected,
        );

        if let RequestSpec::Simple(raw) = spec {
            assert_eq!(raw.method, "GET");
            assert_eq!(raw.endpoint, "/users");
            assert_eq!(raw.base_url, Some("https://api.example.com".to_string()));
        } else {
            panic!("Expected RequestSpec::Simple");
        }
    }

    #[test]
    fn test_build_request_with_path_params() {
        let cmd = mapping::CommandSpec {
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
            args: vec![],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert("id".to_string(), "123".to_string());
        let selected = HashSet::new();
        let spec = build_request_from_command(
            Some("https://api.example.com".to_string()),
            &cmd,
            &vars,
            &selected,
        );

        if let RequestSpec::Simple(raw) = spec {
            assert_eq!(raw.endpoint, "/users/123");
        } else {
            panic!("Expected RequestSpec::Simple");
        }
    }

    #[test]
    fn test_build_request_with_body_template() {
        let cmd = mapping::CommandSpec {
            name: Some("create".to_string()),
            about: None,
            pattern: "users create".to_string(),
            method: Some("POST".to_string()),
            endpoint: Some("/users".to_string()),
            body: Some(r#"{"name": "{name}", "email": "{email}"}"#.to_string()),
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "John".to_string());
        vars.insert("email".to_string(), "john@example.com".to_string());
        let selected = HashSet::new();
        let spec = build_request_from_command(None, &cmd, &vars, &selected);

        if let RequestSpec::Simple(raw) = spec {
            assert_eq!(
                raw.body,
                Some(r#"{"name": "John", "email": "john@example.com"}"#.to_string())
            );
        } else {
            panic!("Expected RequestSpec::Simple");
        }
    }

    #[test]
    fn test_build_request_with_header_template() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer {token}".to_string());

        let cmd = mapping::CommandSpec {
            name: Some("get".to_string()),
            about: None,
            pattern: "api call".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/api".to_string()),
            body: None,
            headers,
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert("token".to_string(), "secret123".to_string());
        let selected = HashSet::new();
        let spec = build_request_from_command(None, &cmd, &vars, &selected);

        if let RequestSpec::Simple(raw) = spec {
            assert!(raw.headers.iter().any(|h| h.contains("Bearer secret123")));
        } else {
            panic!("Expected RequestSpec::Simple");
        }
    }

    #[test]
    fn test_build_request_custom_handler() {
        let cmd = mapping::CommandSpec {
            name: Some("export".to_string()),
            about: None,
            pattern: "export users".to_string(),
            method: None,
            endpoint: None,
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: Some("export_users".to_string()),
            args: vec![],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert("format".to_string(), "csv".to_string());
        let selected = HashSet::new();
        let spec = build_request_from_command(None, &cmd, &vars, &selected);

        if let RequestSpec::CustomHandler {
            handler_name,
            vars: handler_vars,
        } = spec
        {
            assert_eq!(handler_name, "export_users");
            assert_eq!(handler_vars.get("format"), Some(&"csv".to_string()));
            assert!(handler_vars.contains_key("uuid")); // Built-in variable added
        } else {
            panic!("Expected RequestSpec::CustomHandler");
        }
    }

    #[test]
    fn test_build_request_scenario() {
        let scenario = mapping::Scenario {
            scenario_type: "sequential".to_string(),
            steps: vec![mapping::ScenarioStep {
                name: "step1".to_string(),
                method: "POST".to_string(),
                endpoint: "/start".to_string(),
                body: None,
                headers: HashMap::new(),
                extract_response: HashMap::new(),
                polling: None,
            }],
        };

        let cmd = mapping::CommandSpec {
            name: Some("deploy".to_string()),
            about: None,
            pattern: "deploy".to_string(),
            method: None,
            endpoint: None,
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: Some(scenario),
            multipart: false,
            custom_handler: None,
            args: vec![],
            use_common_args: vec![],
        };
        let vars = HashMap::new();
        let selected = HashSet::new();
        let spec = build_request_from_command(
            Some("https://api.example.com".to_string()),
            &cmd,
            &vars,
            &selected,
        );

        if let RequestSpec::Scenario(scenario_spec) = spec {
            assert_eq!(
                scenario_spec.base_url,
                Some("https://api.example.com".to_string())
            );
            assert_eq!(scenario_spec.scenario.steps.len(), 1);
            assert!(scenario_spec.vars.contains_key("uuid")); // Built-in variable added
        } else {
            panic!("Expected RequestSpec::Scenario");
        }
    }

    // ==================== parse_openapi tests ====================

    #[test]
    fn test_parse_openapi_yaml() {
        let yaml = r#"
openapi: "3.0.0"
info:
  title: Test API
  version: "1.0"
servers:
  - url: https://api.example.com
paths: {}
"#;
        let api = parse_openapi(yaml).unwrap();
        assert_eq!(api.info.title, "Test API");
        assert_eq!(api.servers.len(), 1);
        assert_eq!(api.servers[0].url, "https://api.example.com");
    }

    #[test]
    fn test_parse_openapi_json() {
        let json = r#"{
  "openapi": "3.0.0",
  "info": {"title": "Test API", "version": "1.0"},
  "servers": [{"url": "https://api.example.com"}],
  "paths": {}
}"#;
        let api = parse_openapi(json).unwrap();
        assert_eq!(api.info.title, "Test API");
    }

    #[test]
    fn test_parse_openapi_invalid() {
        let invalid = "not valid openapi";
        let result = parse_openapi(invalid);
        assert!(result.is_err());
    }

    // ==================== RawRequestSpec tests ====================

    #[test]
    fn test_raw_request_spec_defaults() {
        let spec = RawRequestSpec {
            base_url: None,
            method: "GET".to_string(),
            endpoint: "/test".to_string(),
            headers: vec![],
            body: None,
            multipart: false,
            file_fields: HashMap::new(),
            table_view: None,
        };
        assert!(spec.base_url.is_none());
        assert!(spec.headers.is_empty());
        assert!(!spec.multipart);
    }

    // ==================== build_url tests ====================

    #[test]
    fn test_build_url_absolute_endpoint() {
        let result = build_url(&None, "https://api.example.com/users");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://api.example.com/users");
    }

    #[test]
    fn test_build_url_http_absolute() {
        let result = build_url(&None, "http://localhost:8080/api");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "http://localhost:8080/api");
    }

    #[test]
    fn test_build_url_relative_with_base() {
        let result = build_url(&Some("https://api.example.com".to_string()), "/users");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://api.example.com/users");
    }

    #[test]
    fn test_build_url_relative_no_base() {
        let result = build_url(&None, "/users");
        assert!(result.is_err());
    }

    #[test]
    fn test_build_url_base_with_trailing_slash() {
        let result = build_url(&Some("https://api.example.com/".to_string()), "/users");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://api.example.com/users");
    }

    #[test]
    fn test_build_url_no_slashes() {
        let result = build_url(&Some("https://api.example.com".to_string()), "users");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://api.example.com/users");
    }

    // ==================== parse_method tests ====================

    #[test]
    fn test_parse_method_get() {
        assert_eq!(parse_method("GET").unwrap(), Method::GET);
        assert_eq!(parse_method("get").unwrap(), Method::GET);
    }

    #[test]
    fn test_parse_method_post() {
        assert_eq!(parse_method("POST").unwrap(), Method::POST);
    }

    #[test]
    fn test_parse_method_put() {
        assert_eq!(parse_method("PUT").unwrap(), Method::PUT);
    }

    #[test]
    fn test_parse_method_patch() {
        assert_eq!(parse_method("PATCH").unwrap(), Method::PATCH);
    }

    #[test]
    fn test_parse_method_delete() {
        assert_eq!(parse_method("DELETE").unwrap(), Method::DELETE);
    }

    #[test]
    fn test_parse_method_head() {
        assert_eq!(parse_method("HEAD").unwrap(), Method::HEAD);
    }

    #[test]
    fn test_parse_method_options() {
        assert_eq!(parse_method("OPTIONS").unwrap(), Method::OPTIONS);
    }

    #[test]
    fn test_parse_method_invalid() {
        let result = parse_method("INVALID");
        assert!(result.is_err());
    }

    // ==================== parse_headers tests ====================

    #[test]
    fn test_parse_headers_valid() {
        let headers = vec!["Content-Type: application/json".to_string()];
        let result = parse_headers(&headers).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn test_parse_headers_multiple() {
        let headers = vec![
            "Content-Type: application/json".to_string(),
            "Authorization: Bearer token123".to_string(),
        ];
        let result = parse_headers(&headers).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_parse_headers_empty() {
        let headers: Vec<String> = vec![];
        let result = parse_headers(&headers).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_headers_invalid_format() {
        let headers = vec!["InvalidHeader".to_string()];
        let result = parse_headers(&headers);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_headers_value_with_colon() {
        let headers = vec!["X-Custom: value:with:colons".to_string()];
        let result = parse_headers(&headers).unwrap();
        assert_eq!(result.get("x-custom").unwrap(), "value:with:colons");
    }

    // ==================== scalar_to_string tests ====================

    #[test]
    fn test_scalar_to_string_null() {
        assert_eq!(scalar_to_string(&serde_json::Value::Null), "null");
    }

    #[test]
    fn test_scalar_to_string_bool() {
        assert_eq!(scalar_to_string(&serde_json::json!(true)), "true");
        assert_eq!(scalar_to_string(&serde_json::json!(false)), "false");
    }

    #[test]
    fn test_scalar_to_string_number() {
        assert_eq!(scalar_to_string(&serde_json::json!(42)), "42");
        assert_eq!(scalar_to_string(&serde_json::json!(3.15)), "3.15");
    }

    #[test]
    fn test_scalar_to_string_string() {
        assert_eq!(scalar_to_string(&serde_json::json!("hello")), "hello");
    }

    #[test]
    fn test_scalar_to_string_object() {
        let obj = serde_json::json!({"key": "value"});
        let result = scalar_to_string(&obj);
        assert!(result.contains("key"));
    }

    // ==================== humanize_column_label tests ====================

    #[test]
    fn test_humanize_column_label_simple() {
        assert_eq!(humanize_column_label("user_name"), "User Name");
    }

    #[test]
    fn test_humanize_column_label_with_path() {
        assert_eq!(humanize_column_label("user.first_name"), "First Name");
    }

    #[test]
    fn test_humanize_column_label_dashes() {
        assert_eq!(humanize_column_label("created-at"), "Created At");
    }

    #[test]
    fn test_humanize_column_label_mixed() {
        assert_eq!(humanize_column_label("user_id-value"), "User Id Value");
    }

    // ==================== parse_column_spec tests ====================

    #[test]
    fn test_parse_column_spec_simple() {
        let spec = parse_column_spec("user_name");
        assert_eq!(spec.path, "user_name");
        assert!(spec.modifier.is_none());
    }

    #[test]
    fn test_parse_column_spec_with_gb() {
        let spec = parse_column_spec("size:gb");
        assert_eq!(spec.path, "size");
        assert!(matches!(spec.modifier, Some(SizeModifier::Gigabytes)));
    }

    #[test]
    fn test_parse_column_spec_with_mb() {
        let spec = parse_column_spec("size:mb");
        assert_eq!(spec.path, "size");
        assert!(matches!(spec.modifier, Some(SizeModifier::Megabytes)));
    }

    #[test]
    fn test_parse_column_spec_with_kb() {
        let spec = parse_column_spec("size:kb");
        assert_eq!(spec.path, "size");
        assert!(matches!(spec.modifier, Some(SizeModifier::Kilobytes)));
    }

    #[test]
    fn test_parse_column_spec_unknown_modifier() {
        let spec = parse_column_spec("size:unknown");
        assert_eq!(spec.path, "size");
        assert!(spec.modifier.is_none());
    }

    // ==================== get_value_by_path tests ====================

    #[test]
    fn test_get_value_by_path_simple() {
        let json = serde_json::json!({"name": "John"});
        let result = get_value_by_path(&json, "name");
        assert_eq!(result, &serde_json::json!("John"));
    }

    #[test]
    fn test_get_value_by_path_nested() {
        let json = serde_json::json!({"user": {"name": "John"}});
        let result = get_value_by_path(&json, "user.name");
        assert_eq!(result, &serde_json::json!("John"));
    }

    #[test]
    fn test_get_value_by_path_missing() {
        let json = serde_json::json!({"name": "John"});
        let result = get_value_by_path(&json, "missing");
        assert_eq!(result, &serde_json::Value::Null);
    }

    #[test]
    fn test_get_value_by_path_non_object() {
        let json = serde_json::json!("string");
        let result = get_value_by_path(&json, "field");
        assert_eq!(result, &serde_json::Value::Null);
    }

    // ==================== scalar_to_string_with_modifier tests ====================

    #[test]
    fn test_scalar_to_string_with_modifier_gb() {
        let bytes = serde_json::json!(1073741824); // 1 GB
        let result = scalar_to_string_with_modifier(&bytes, &Some(SizeModifier::Gigabytes));
        assert_eq!(result, "1.00");
    }

    #[test]
    fn test_scalar_to_string_with_modifier_mb() {
        let bytes = serde_json::json!(1048576); // 1 MB
        let result = scalar_to_string_with_modifier(&bytes, &Some(SizeModifier::Megabytes));
        assert_eq!(result, "1.00");
    }

    #[test]
    fn test_scalar_to_string_with_modifier_kb() {
        let bytes = serde_json::json!(1024); // 1 KB
        let result = scalar_to_string_with_modifier(&bytes, &Some(SizeModifier::Kilobytes));
        assert_eq!(result, "1.00");
    }

    #[test]
    fn test_scalar_to_string_with_modifier_string_number() {
        let bytes = serde_json::json!("1048576"); // 1 MB as string
        let result = scalar_to_string_with_modifier(&bytes, &Some(SizeModifier::Megabytes));
        assert_eq!(result, "1.00");
    }

    #[test]
    fn test_scalar_to_string_with_modifier_non_numeric_string() {
        let value = serde_json::json!("not a number");
        let result = scalar_to_string_with_modifier(&value, &Some(SizeModifier::Megabytes));
        assert_eq!(result, "not a number");
    }

    #[test]
    fn test_scalar_to_string_with_modifier_none() {
        let value = serde_json::json!(42);
        let result = scalar_to_string_with_modifier(&value, &None);
        assert_eq!(result, "42");
    }

    #[test]
    fn test_scalar_to_string_with_modifier_null() {
        let value = serde_json::Value::Null;
        let result = scalar_to_string_with_modifier(&value, &Some(SizeModifier::Gigabytes));
        assert_eq!(result, "null");
    }

    // ==================== humanize_column_label_with_modifier tests ====================

    #[test]
    fn test_humanize_column_label_with_modifier_gb() {
        let result =
            humanize_column_label_with_modifier("disk_size", &Some(SizeModifier::Gigabytes));
        assert!(result.contains("Disk Size"));
        assert!(result.contains("GB"));
    }

    #[test]
    fn test_humanize_column_label_with_modifier_mb() {
        let result = humanize_column_label_with_modifier("memory", &Some(SizeModifier::Megabytes));
        assert!(result.contains("Memory"));
        assert!(result.contains("MB"));
    }

    #[test]
    fn test_humanize_column_label_with_modifier_kb() {
        let result =
            humanize_column_label_with_modifier("cache_size", &Some(SizeModifier::Kilobytes));
        assert!(result.contains("Cache Size"));
        assert!(result.contains("KB"));
    }

    #[test]
    fn test_humanize_column_label_with_modifier_none() {
        let result = humanize_column_label_with_modifier("user_name", &None);
        assert_eq!(result, "User Name");
    }

    // ==================== apply_file_overrides tests ====================

    #[test]
    fn test_apply_file_overrides_no_file_args() {
        let args = vec![mapping::ArgSpec {
            name: Some("name".to_string()),
            arg_type: None,
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "John".to_string());
        apply_file_overrides(&args, &mut vars);
        assert_eq!(vars.get("name"), Some(&"John".to_string()));
    }

    #[test]
    fn test_apply_file_overrides_file_arg_empty_path() {
        let args = vec![mapping::ArgSpec {
            name: Some("config_file".to_string()),
            arg_type: Some("file".to_string()),
            file_overrides_value_of: Some("config".to_string()),
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        vars.insert("config_file".to_string(), "".to_string());
        apply_file_overrides(&args, &mut vars);
        assert!(!vars.contains_key("config"));
    }

    #[test]
    fn test_apply_file_overrides_missing_file() {
        let args = vec![mapping::ArgSpec {
            name: Some("config_file".to_string()),
            arg_type: Some("file".to_string()),
            file_overrides_value_of: Some("config".to_string()),
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        vars.insert(
            "config_file".to_string(),
            "/nonexistent/path/file.txt".to_string(),
        );
        apply_file_overrides(&args, &mut vars);
        // Should not insert config since file doesn't exist
        assert!(!vars.contains_key("config"));
    }

    // ==================== extract_jsonpath_value tests ====================

    #[test]
    fn test_extract_jsonpath_value_string() {
        let json = serde_json::json!({"name": "John"});
        let result = extract_jsonpath_value(&json, "$.name");
        assert_eq!(result, Some("John".to_string()));
    }

    #[test]
    fn test_extract_jsonpath_value_number() {
        let json = serde_json::json!({"age": 30});
        let result = extract_jsonpath_value(&json, "$.age");
        assert_eq!(result, Some("30".to_string()));
    }

    #[test]
    fn test_extract_jsonpath_value_bool() {
        let json = serde_json::json!({"active": true});
        let result = extract_jsonpath_value(&json, "$.active");
        assert_eq!(result, Some("true".to_string()));
    }

    #[test]
    fn test_extract_jsonpath_value_nested() {
        let json = serde_json::json!({"user": {"id": 123}});
        let result = extract_jsonpath_value(&json, "$.user.id");
        assert_eq!(result, Some("123".to_string()));
    }

    #[test]
    fn test_extract_jsonpath_value_missing() {
        let json = serde_json::json!({"name": "John"});
        let result = extract_jsonpath_value(&json, "$.missing");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_jsonpath_value_object() {
        let json = serde_json::json!({"user": {"name": "John"}});
        let result = extract_jsonpath_value(&json, "$.user");
        assert!(result.is_some());
        assert!(result.unwrap().contains("John"));
    }

    // ==================== build_request_from_command edge cases ====================

    #[test]
    fn test_build_request_with_arg_overrides() {
        let cmd = mapping::CommandSpec {
            name: Some("test".to_string()),
            about: None,
            pattern: "test".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/default".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![mapping::ArgSpec {
                name: Some("override_arg".to_string()),
                endpoint: Some("/overridden".to_string()),
                method: Some("POST".to_string()),
                ..Default::default()
            }],
            use_common_args: vec![],
        };
        let vars = HashMap::new();
        let mut selected = HashSet::new();
        selected.insert("override_arg".to_string());
        let spec = build_request_from_command(
            Some("https://api.example.com".to_string()),
            &cmd,
            &vars,
            &selected,
        );

        if let RequestSpec::Simple(raw) = spec {
            assert_eq!(raw.endpoint, "/overridden");
            assert_eq!(raw.method, "POST");
        } else {
            panic!("Expected RequestSpec::Simple");
        }
    }

    #[test]
    fn test_build_request_multipart() {
        let cmd = mapping::CommandSpec {
            name: Some("upload".to_string()),
            about: None,
            pattern: "upload".to_string(),
            method: Some("POST".to_string()),
            endpoint: Some("/upload".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: true,
            custom_handler: None,
            args: vec![mapping::ArgSpec {
                name: Some("file".to_string()),
                file_upload: true,
                ..Default::default()
            }],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert("file".to_string(), "/path/to/file.txt".to_string());
        let selected = HashSet::new();
        let spec = build_request_from_command(None, &cmd, &vars, &selected);

        if let RequestSpec::Simple(raw) = spec {
            assert!(raw.multipart);
            assert!(raw.file_fields.contains_key("file"));
        } else {
            panic!("Expected RequestSpec::Simple");
        }
    }

    // ==================== extract_response_variables tests ====================

    #[test]
    fn test_extract_response_variables_empty() {
        let extractions = HashMap::new();
        let mut vars = HashMap::new();
        let result = extract_response_variables("{}", &extractions, &mut vars);
        assert!(result.is_ok());
    }

    #[test]
    fn test_extract_response_variables_success() {
        let mut extractions = HashMap::new();
        extractions.insert("user_id".to_string(), "$.id".to_string());
        let mut vars = HashMap::new();
        let result = extract_response_variables(r#"{"id": "123"}"#, &extractions, &mut vars);
        assert!(result.is_ok());
        assert_eq!(vars.get("user_id"), Some(&"123".to_string()));
    }

    #[test]
    fn test_extract_response_variables_invalid_json() {
        let mut extractions = HashMap::new();
        extractions.insert("user_id".to_string(), "$.id".to_string());
        let mut vars = HashMap::new();
        let result = extract_response_variables("not json", &extractions, &mut vars);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_response_variables_missing_path() {
        let mut extractions = HashMap::new();
        extractions.insert("user_id".to_string(), "$.missing".to_string());
        let mut vars = HashMap::new();
        let result = extract_response_variables(r#"{"id": "123"}"#, &extractions, &mut vars);
        assert!(result.is_err());
    }

    // ==================== build_raw_spec_from_step tests ====================

    #[test]
    fn test_build_raw_spec_from_step() {
        let step = mapping::ScenarioStep {
            name: "test_step".to_string(),
            method: "POST".to_string(),
            endpoint: "/api/{id}".to_string(),
            body: Some(r#"{"name": "{name}"}"#.to_string()),
            headers: HashMap::new(),
            extract_response: HashMap::new(),
            polling: None,
        };
        let mut vars = HashMap::new();
        vars.insert("id".to_string(), "123".to_string());
        vars.insert("name".to_string(), "Test".to_string());

        let result =
            build_raw_spec_from_step(&Some("https://api.example.com".to_string()), &step, &vars);
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert_eq!(spec.method, "POST");
        assert_eq!(spec.endpoint, "/api/123");
        assert_eq!(spec.body, Some(r#"{"name": "Test"}"#.to_string()));
    }

    #[test]
    fn test_build_raw_spec_from_step_with_headers() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer {token}".to_string());
        let step = mapping::ScenarioStep {
            name: "test_step".to_string(),
            method: "GET".to_string(),
            endpoint: "/api".to_string(),
            body: None,
            headers,
            extract_response: HashMap::new(),
            polling: None,
        };
        let mut vars = HashMap::new();
        vars.insert("token".to_string(), "secret".to_string());

        let result = build_raw_spec_from_step(&None, &step, &vars);
        assert!(result.is_ok());
        let spec = result.unwrap();
        assert!(spec.headers.iter().any(|h| h.contains("Bearer secret")));
    }

    // ==================== print_human_readable tests ====================

    #[test]
    fn test_print_human_readable_object() {
        let json = serde_json::json!({
            "name": "John",
            "age": 30,
            "active": true
        });
        // Just verify it doesn't panic
        print_human_readable(&json, None);
    }

    #[test]
    fn test_print_human_readable_array() {
        let json = serde_json::json!([
            {"id": 1, "name": "Alice"},
            {"id": 2, "name": "Bob"}
        ]);
        // Just verify it doesn't panic
        print_human_readable(&json, None);
    }

    #[test]
    fn test_print_human_readable_scalar() {
        let json = serde_json::json!("simple string");
        print_human_readable(&json, None);

        let json = serde_json::json!(42);
        print_human_readable(&json, None);

        let json = serde_json::json!(true);
        print_human_readable(&json, None);
    }

    #[test]
    fn test_print_human_readable_object_with_nested_array() {
        let json = serde_json::json!({
            "total": 2,
            "users": [
                {"id": 1, "name": "Alice"},
                {"id": 2, "name": "Bob"}
            ]
        });
        print_human_readable(&json, None);
    }

    #[test]
    fn test_print_human_readable_with_table_view() {
        let json = serde_json::json!([
            {"id": 1, "name": "Alice", "email": "alice@example.com"},
            {"id": 2, "name": "Bob", "email": "bob@example.com"}
        ]);
        let table_view = vec!["id".to_string(), "name".to_string()];
        print_human_readable(&json, Some(&table_view));
    }

    // ==================== print_array_table tests ====================

    #[test]
    fn test_print_array_table_empty() {
        let arr: Vec<serde_json::Value> = vec![];
        print_array_table(&arr, None);
    }

    #[test]
    fn test_print_array_table_simple() {
        let arr = vec![
            serde_json::json!({"id": 1, "name": "Alice"}),
            serde_json::json!({"id": 2, "name": "Bob"}),
        ];
        print_array_table(&arr, None);
    }

    #[test]
    fn test_print_array_table_with_nested_objects() {
        let arr = vec![
            serde_json::json!({"id": 1, "user": {"name": "Alice"}}),
            serde_json::json!({"id": 2, "user": {"name": "Bob"}}),
        ];
        print_array_table(&arr, None);
    }

    #[test]
    fn test_print_array_table_with_column_spec() {
        let arr = vec![
            serde_json::json!({"id": 1, "name": "Alice", "size": 1073741824_i64}),
            serde_json::json!({"id": 2, "name": "Bob", "size": 2147483648_i64}),
        ];
        let cols = vec!["id".to_string(), "name".to_string(), "size:gb".to_string()];
        print_array_table(&arr, Some(&cols));
    }

    #[test]
    fn test_print_array_table_scalars() {
        let arr = vec![
            serde_json::json!("item1"),
            serde_json::json!("item2"),
            serde_json::json!("item3"),
        ];
        print_array_table(&arr, None);
    }

    // ==================== RequestSpec tests ====================

    #[test]
    fn test_request_spec_simple_clone() {
        let spec = RequestSpec::Simple(RawRequestSpec {
            base_url: Some("https://api.example.com".to_string()),
            method: "GET".to_string(),
            endpoint: "/users".to_string(),
            headers: vec!["Content-Type: application/json".to_string()],
            body: None,
            multipart: false,
            file_fields: HashMap::new(),
            table_view: None,
        });
        let cloned = spec.clone();
        if let RequestSpec::Simple(raw) = cloned {
            assert_eq!(raw.method, "GET");
        }
    }

    #[test]
    fn test_request_spec_scenario_clone() {
        let scenario = mapping::Scenario {
            scenario_type: "sequential".to_string(),
            steps: vec![],
        };
        let spec = RequestSpec::Scenario(ScenarioSpec {
            base_url: Some("https://api.example.com".to_string()),
            scenario,
            vars: HashMap::new(),
        });
        let cloned = spec.clone();
        assert!(matches!(cloned, RequestSpec::Scenario(_)));
    }

    #[test]
    fn test_request_spec_custom_handler_clone() {
        let mut vars = HashMap::new();
        vars.insert("key".to_string(), "value".to_string());
        let spec = RequestSpec::CustomHandler {
            handler_name: "test_handler".to_string(),
            vars,
        };
        let cloned = spec.clone();
        if let RequestSpec::CustomHandler { handler_name, vars } = cloned {
            assert_eq!(handler_name, "test_handler");
            assert_eq!(vars.get("key"), Some(&"value".to_string()));
        }
    }

    // ==================== ScenarioSpec tests ====================

    #[test]
    fn test_scenario_spec_debug() {
        let scenario = mapping::Scenario {
            scenario_type: "job_with_polling".to_string(),
            steps: vec![],
        };
        let spec = ScenarioSpec {
            base_url: Some("https://api.example.com".to_string()),
            scenario,
            vars: HashMap::new(),
        };
        let debug_str = format!("{:?}", spec);
        assert!(debug_str.contains("ScenarioSpec"));
    }

    // ==================== ExecutionConfig tests ====================

    #[test]
    fn test_execution_config_clone() {
        let config = ExecutionConfig::new("test-agent");
        let cloned = config.clone();
        assert_eq!(cloned.user_agent, "test-agent");
    }

    #[test]
    fn test_execution_config_debug() {
        let config = ExecutionConfig::new("test-agent");
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("ExecutionConfig"));
    }

    // ==================== build_request_from_command with header overrides ====================

    #[test]
    fn test_build_request_with_arg_header_override() {
        let mut arg_headers = HashMap::new();
        arg_headers.insert("X-Custom".to_string(), "custom-value".to_string());

        let cmd = mapping::CommandSpec {
            name: Some("test".to_string()),
            about: None,
            pattern: "test".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/test".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![mapping::ArgSpec {
                name: Some("custom_arg".to_string()),
                headers: Some(arg_headers),
                body: Some(r#"{"override": true}"#.to_string()),
                ..Default::default()
            }],
            use_common_args: vec![],
        };
        let vars = HashMap::new();
        let mut selected = HashSet::new();
        selected.insert("custom_arg".to_string());
        let spec = build_request_from_command(
            Some("https://api.example.com".to_string()),
            &cmd,
            &vars,
            &selected,
        );

        if let RequestSpec::Simple(raw) = spec {
            assert!(raw.headers.iter().any(|h| h.contains("X-Custom")));
            assert_eq!(raw.body, Some(r#"{"override": true}"#.to_string()));
        } else {
            panic!("Expected RequestSpec::Simple");
        }
    }

    // ==================== apply_file_overrides with real file ====================

    #[test]
    fn test_apply_file_overrides_with_real_file() {
        use std::io::Write;

        // Create a temp file
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("rclib_test_file.txt");
        let mut file = std::fs::File::create(&temp_file).unwrap();
        writeln!(file, "file content here").unwrap();
        drop(file);

        let args = vec![mapping::ArgSpec {
            name: Some("config_file".to_string()),
            arg_type: Some("file".to_string()),
            file_overrides_value_of: Some("config".to_string()),
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        vars.insert(
            "config_file".to_string(),
            temp_file.to_string_lossy().to_string(),
        );

        apply_file_overrides(&args, &mut vars);

        assert!(vars.contains_key("config"));
        assert!(vars.get("config").unwrap().contains("file content here"));

        // Cleanup
        let _ = std::fs::remove_file(temp_file);
    }

    // ==================== OutputFormat tests ====================

    #[test]
    fn test_output_format_debug() {
        let format = OutputFormat::Json;
        let debug_str = format!("{:?}", format);
        assert!(debug_str.contains("Json"));
    }

    #[test]
    fn test_output_format_clone() {
        let format = OutputFormat::Human;
        let copied = format;
        assert_eq!(copied, OutputFormat::Human);
    }

    #[test]
    fn test_output_format_copy() {
        let format = OutputFormat::Quiet;
        let copied: OutputFormat = format;
        assert_eq!(copied, OutputFormat::Quiet);
    }

    // ==================== RawRequestSpec tests ====================

    #[test]
    fn test_raw_request_spec_clone() {
        let spec = RawRequestSpec {
            base_url: Some("https://api.example.com".to_string()),
            method: "POST".to_string(),
            endpoint: "/users".to_string(),
            headers: vec!["Content-Type: application/json".to_string()],
            body: Some(r#"{"name": "test"}"#.to_string()),
            multipart: false,
            file_fields: HashMap::new(),
            table_view: Some(vec!["id".to_string(), "name".to_string()]),
        };
        let cloned = spec.clone();
        assert_eq!(cloned.method, "POST");
        assert_eq!(cloned.body, Some(r#"{"name": "test"}"#.to_string()));
        assert!(cloned.table_view.is_some());
    }

    #[test]
    fn test_raw_request_spec_debug() {
        let spec = RawRequestSpec {
            base_url: None,
            method: "GET".to_string(),
            endpoint: "/test".to_string(),
            headers: vec![],
            body: None,
            multipart: false,
            file_fields: HashMap::new(),
            table_view: None,
        };
        let debug_str = format!("{:?}", spec);
        assert!(debug_str.contains("RawRequestSpec"));
        assert!(debug_str.contains("GET"));
    }

    // ==================== build_request edge cases ====================

    #[test]
    fn test_build_request_with_table_view() {
        let cmd = mapping::CommandSpec {
            name: Some("list".to_string()),
            about: None,
            pattern: "users list".to_string(),
            method: Some("GET".to_string()),
            endpoint: Some("/users".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: Some(vec![
                "id".to_string(),
                "name".to_string(),
                "email".to_string(),
            ]),
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![],
            use_common_args: vec![],
        };
        let vars = HashMap::new();
        let selected = HashSet::new();
        let spec = build_request_from_command(None, &cmd, &vars, &selected);

        if let RequestSpec::Simple(raw) = spec {
            assert!(raw.table_view.is_some());
            let tv = raw.table_view.unwrap();
            assert_eq!(tv.len(), 3);
            assert!(tv.contains(&"id".to_string()));
        } else {
            panic!("Expected RequestSpec::Simple");
        }
    }

    #[test]
    fn test_build_request_multipart_without_file_upload() {
        // Multipart true but no file_upload args
        let cmd = mapping::CommandSpec {
            name: Some("upload".to_string()),
            about: None,
            pattern: "upload".to_string(),
            method: Some("POST".to_string()),
            endpoint: Some("/upload".to_string()),
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: true,
            custom_handler: None,
            args: vec![mapping::ArgSpec {
                name: Some("description".to_string()),
                file_upload: false, // Not a file upload
                ..Default::default()
            }],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert("description".to_string(), "test description".to_string());
        let selected = HashSet::new();
        let spec = build_request_from_command(None, &cmd, &vars, &selected);

        if let RequestSpec::Simple(raw) = spec {
            assert!(raw.multipart);
            assert!(raw.file_fields.is_empty()); // No file fields
        } else {
            panic!("Expected RequestSpec::Simple");
        }
    }

    // ==================== Scenario edge cases ====================

    #[test]
    fn test_build_request_scenario_with_vars() {
        let scenario = mapping::Scenario {
            scenario_type: "job_with_polling".to_string(),
            steps: vec![
                mapping::ScenarioStep {
                    name: "schedule_job".to_string(),
                    method: "POST".to_string(),
                    endpoint: "/jobs".to_string(),
                    body: Some(r#"{"name": "{job_name}"}"#.to_string()),
                    headers: HashMap::new(),
                    extract_response: HashMap::new(),
                    polling: None,
                },
                mapping::ScenarioStep {
                    name: "poll_job".to_string(),
                    method: "GET".to_string(),
                    endpoint: "/jobs/{job_id}".to_string(),
                    body: None,
                    headers: HashMap::new(),
                    extract_response: HashMap::new(),
                    polling: Some(mapping::PollingConfig {
                        interval_seconds: 5,
                        timeout_seconds: 300,
                        completion_conditions: vec![],
                    }),
                },
            ],
        };

        let cmd = mapping::CommandSpec {
            name: Some("run_job".to_string()),
            about: None,
            pattern: "run job".to_string(),
            method: None,
            endpoint: None,
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: Some(scenario),
            multipart: false,
            custom_handler: None,
            args: vec![],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert("job_name".to_string(), "test_job".to_string());
        let selected = HashSet::new();
        let spec = build_request_from_command(
            Some("https://api.example.com".to_string()),
            &cmd,
            &vars,
            &selected,
        );

        if let RequestSpec::Scenario(scenario_spec) = spec {
            assert_eq!(scenario_spec.scenario.scenario_type, "job_with_polling");
            assert_eq!(scenario_spec.scenario.steps.len(), 2);
            assert!(scenario_spec.vars.contains_key("job_name"));
            assert!(scenario_spec.vars.contains_key("uuid")); // Built-in
        } else {
            panic!("Expected RequestSpec::Scenario");
        }
    }

    // ==================== Custom handler edge cases ====================

    #[test]
    fn test_build_request_custom_handler_with_file_override() {
        use std::io::Write;

        // Create a temp file
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("rclib_handler_test.txt");
        let mut file = std::fs::File::create(&temp_file).unwrap();
        writeln!(file, "handler file content").unwrap();
        drop(file);

        let cmd = mapping::CommandSpec {
            name: Some("process".to_string()),
            about: None,
            pattern: "process".to_string(),
            method: None,
            endpoint: None,
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: Some("process_handler".to_string()),
            args: vec![mapping::ArgSpec {
                name: Some("input_file".to_string()),
                arg_type: Some("file".to_string()),
                file_overrides_value_of: Some("input_content".to_string()),
                ..Default::default()
            }],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert(
            "input_file".to_string(),
            temp_file.to_string_lossy().to_string(),
        );
        let selected = HashSet::new();
        let spec = build_request_from_command(None, &cmd, &vars, &selected);

        if let RequestSpec::CustomHandler {
            handler_name,
            vars: handler_vars,
        } = spec
        {
            assert_eq!(handler_name, "process_handler");
            assert!(handler_vars.contains_key("input_content"));
            assert!(handler_vars
                .get("input_content")
                .unwrap()
                .contains("handler file content"));
        } else {
            panic!("Expected RequestSpec::CustomHandler");
        }

        // Cleanup
        let _ = std::fs::remove_file(temp_file);
    }

    // ==================== ExecutionResult tests ====================

    #[test]
    fn test_execution_result_debug() {
        let result = ExecutionResult {
            duration: std::time::Duration::from_millis(100),
            is_success: true,
        };
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("ExecutionResult"));
        assert!(debug_str.contains("100"));
    }

    #[test]
    fn test_execution_result_clone() {
        let result = ExecutionResult {
            duration: std::time::Duration::from_secs(1),
            is_success: false,
        };
        let cloned = result.clone();
        assert!(!cloned.is_success);
        assert_eq!(cloned.duration.as_secs(), 1);
    }

    // ==================== ColumnSpec tests ====================

    #[test]
    fn test_column_spec_debug() {
        let spec = ColumnSpec {
            path: "user.name".to_string(),
            modifier: Some(SizeModifier::Megabytes),
        };
        let debug_str = format!("{:?}", spec);
        assert!(debug_str.contains("ColumnSpec"));
        assert!(debug_str.contains("user.name"));
    }

    #[test]
    fn test_column_spec_clone() {
        let spec = ColumnSpec {
            path: "size".to_string(),
            modifier: Some(SizeModifier::Gigabytes),
        };
        let cloned = spec.clone();
        assert_eq!(cloned.path, "size");
        assert!(matches!(cloned.modifier, Some(SizeModifier::Gigabytes)));
    }

    // ==================== SizeModifier tests ====================

    #[test]
    fn test_size_modifier_debug() {
        let gb = SizeModifier::Gigabytes;
        let mb = SizeModifier::Megabytes;
        let kb = SizeModifier::Kilobytes;

        assert!(format!("{:?}", gb).contains("Gigabytes"));
        assert!(format!("{:?}", mb).contains("Megabytes"));
        assert!(format!("{:?}", kb).contains("Kilobytes"));
    }

    #[test]
    fn test_size_modifier_clone() {
        let modifier = SizeModifier::Kilobytes;
        let cloned = modifier.clone();
        assert!(matches!(cloned, SizeModifier::Kilobytes));
    }

    // ==================== build_request with file overrides in simple request ====================

    #[test]
    fn test_build_request_simple_with_file_override() {
        use std::io::Write;

        // Create a temp file
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("rclib_simple_test.json");
        let mut file = std::fs::File::create(&temp_file).unwrap();
        writeln!(file, r#"{{"data": "from file"}}"#).unwrap();
        drop(file);

        let cmd = mapping::CommandSpec {
            name: Some("create".to_string()),
            about: None,
            pattern: "create".to_string(),
            method: Some("POST".to_string()),
            endpoint: Some("/items".to_string()),
            body: Some("{body}".to_string()),
            headers: HashMap::new(),
            table_view: None,
            scenario: None,
            multipart: false,
            custom_handler: None,
            args: vec![mapping::ArgSpec {
                name: Some("body_file".to_string()),
                arg_type: Some("file".to_string()),
                file_overrides_value_of: Some("body".to_string()),
                ..Default::default()
            }],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert(
            "body_file".to_string(),
            temp_file.to_string_lossy().to_string(),
        );
        let selected = HashSet::new();
        let spec = build_request_from_command(
            Some("https://api.example.com".to_string()),
            &cmd,
            &vars,
            &selected,
        );

        if let RequestSpec::Simple(raw) = spec {
            assert!(raw.body.is_some());
            assert!(raw.body.unwrap().contains("from file"));
        } else {
            panic!("Expected RequestSpec::Simple");
        }

        // Cleanup
        let _ = std::fs::remove_file(temp_file);
    }

    // ==================== build_request with scenario file overrides ====================

    #[test]
    fn test_build_request_scenario_with_file_override() {
        use std::io::Write;

        // Create a temp file
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("rclib_scenario_test.json");
        let mut file = std::fs::File::create(&temp_file).unwrap();
        writeln!(file, r#"scenario config content"#).unwrap();
        drop(file);

        let scenario = mapping::Scenario {
            scenario_type: "job_with_polling".to_string(),
            steps: vec![
                mapping::ScenarioStep {
                    name: "schedule_job".to_string(),
                    method: "POST".to_string(),
                    endpoint: "/jobs".to_string(),
                    body: Some("{config}".to_string()),
                    headers: HashMap::new(),
                    extract_response: HashMap::new(),
                    polling: None,
                },
                mapping::ScenarioStep {
                    name: "poll_job".to_string(),
                    method: "GET".to_string(),
                    endpoint: "/jobs/{job_id}".to_string(),
                    body: None,
                    headers: HashMap::new(),
                    extract_response: HashMap::new(),
                    polling: Some(mapping::PollingConfig {
                        interval_seconds: 1,
                        timeout_seconds: 10,
                        completion_conditions: vec![],
                    }),
                },
            ],
        };

        let cmd = mapping::CommandSpec {
            name: Some("run".to_string()),
            about: None,
            pattern: "run".to_string(),
            method: None,
            endpoint: None,
            body: None,
            headers: HashMap::new(),
            table_view: None,
            scenario: Some(scenario),
            multipart: false,
            custom_handler: None,
            args: vec![mapping::ArgSpec {
                name: Some("config_file".to_string()),
                arg_type: Some("file".to_string()),
                file_overrides_value_of: Some("config".to_string()),
                ..Default::default()
            }],
            use_common_args: vec![],
        };
        let mut vars = HashMap::new();
        vars.insert(
            "config_file".to_string(),
            temp_file.to_string_lossy().to_string(),
        );
        let selected = HashSet::new();
        let spec = build_request_from_command(
            Some("https://api.example.com".to_string()),
            &cmd,
            &vars,
            &selected,
        );

        if let RequestSpec::Scenario(scenario_spec) = spec {
            assert!(scenario_spec.vars.contains_key("config"));
            assert!(scenario_spec
                .vars
                .get("config")
                .unwrap()
                .contains("scenario config content"));
        } else {
            panic!("Expected RequestSpec::Scenario");
        }

        // Cleanup
        let _ = std::fs::remove_file(temp_file);
    }

    // ==================== humanize_column_label edge cases ====================

    #[test]
    fn test_humanize_column_label_empty() {
        let result = humanize_column_label("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_humanize_column_label_single_char() {
        let result = humanize_column_label("x");
        assert_eq!(result, "X");
    }

    #[test]
    fn test_humanize_column_label_all_uppercase() {
        let result = humanize_column_label("USER_ID");
        assert_eq!(result, "User Id");
    }

    // ==================== get_value_by_path edge cases ====================

    #[test]
    fn test_get_value_by_path_deeply_nested() {
        let json = serde_json::json!({
            "level1": {
                "level2": {
                    "level3": {
                        "value": "deep"
                    }
                }
            }
        });
        let result = get_value_by_path(&json, "level1.level2.level3.value");
        assert_eq!(result, &serde_json::json!("deep"));
    }

    #[test]
    fn test_get_value_by_path_array_value() {
        let json = serde_json::json!({
            "items": [1, 2, 3]
        });
        let result = get_value_by_path(&json, "items");
        assert!(result.is_array());
    }

    // ==================== print_array_table edge cases ====================

    #[test]
    fn test_print_array_table_mixed_types() {
        let arr = vec![
            serde_json::json!({"id": 1, "value": "string"}),
            serde_json::json!({"id": 2, "value": 42}),
            serde_json::json!({"id": 3, "value": true}),
            serde_json::json!({"id": 4, "value": null}),
        ];
        print_array_table(&arr, None);
    }

    #[test]
    fn test_print_array_table_with_all_modifiers() {
        let arr =
            vec![serde_json::json!({"gb": 1073741824_i64, "mb": 1048576_i64, "kb": 1024_i64})];
        let cols = vec![
            "gb:gb".to_string(),
            "mb:mb".to_string(),
            "kb:kb".to_string(),
        ];
        print_array_table(&arr, Some(&cols));
    }

    // ==================== substitute_template edge cases ====================

    #[test]
    fn test_substitute_template_special_chars() {
        let mut vars = HashMap::new();
        vars.insert("query".to_string(), "hello world".to_string());
        let result = substitute_template("/search?q={query}", &vars);
        assert_eq!(result, "/search?q=hello world");
    }

    #[test]
    fn test_substitute_template_underscore_var() {
        let mut vars = HashMap::new();
        vars.insert("user_id".to_string(), "123".to_string());
        vars.insert("org_name".to_string(), "acme".to_string());
        let result = substitute_template("/orgs/{org_name}/users/{user_id}", &vars);
        assert_eq!(result, "/orgs/acme/users/123");
    }

    #[test]
    fn test_substitute_template_numeric_suffix() {
        let mut vars = HashMap::new();
        vars.insert("param1".to_string(), "a".to_string());
        vars.insert("param2".to_string(), "b".to_string());
        let result = substitute_template("{param1}-{param2}", &vars);
        assert_eq!(result, "a-b");
    }
}

// HTTP tests require a running mock server - moved to integration tests
// to avoid async/blocking conflicts with the blocking reqwest client
