use std::env;
use std::fs;
use std::collections::HashMap;

use anyhow::{Context, Result};

const EMBEDDED_OPENAPI: &str = include_str!("dummyjson-openapi-spec.yaml");
const EMBEDDED_MAPPING: &str = include_str!("mapping.yaml");
const APP_NAME: &str = "dummyjson-cli";

// Register app-level custom handlers and delegate driving to rclib

// (no per-command handlers here; registered below)

fn main() {
    if let Err(err) = real_main() {
        eprintln!("Error: {:#}", err);
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    // Pre-scan for optional external files
    let args: Vec<String> = env::args().collect();
    let mapping_file = rclib::cli::pre_scan_value(&args, "--mapping-file");
    let openapi_file = rclib::cli::pre_scan_value(&args, "--openapi-file");

    // Load OpenAPI (for default base URL)
    let openapi_text = if let Some(path) = openapi_file.as_deref() {
        fs::read_to_string(path).with_context(|| format!("Failed to read openapi file: {}", path))?
    } else {
        EMBEDDED_OPENAPI.to_string()
    };
    let openapi = rclib::parse_openapi(&openapi_text).context("OpenAPI parsing failed")?;
    let default_base_url = openapi.servers.first().map(|s| s.url.clone()).unwrap_or_else(|| {
        "https://dummyjson.com".to_string()
    });

    // Load mapping used to build the dynamic command tree
    let mapping_yaml = if let Some(path) = mapping_file.as_deref() {
        fs::read_to_string(path).with_context(|| format!("Failed to read mapping file: {}", path))?
    } else {
        EMBEDDED_MAPPING.to_string()
    };
    let mapping_root = rclib::mapping::parse_mapping_root(&mapping_yaml)?;

    // Build CLI
    let (app, _) = rclib::cli::build_cli(&mapping_root, &default_base_url);
    let matches = app.get_matches();

    // Global options are handled by rclib::cli::drive_command

    // Register custom handlers
    let mut reg = rclib::cli::HandlerRegistry::new();
    reg.register("export_users", |vars, base_url, json_output| {
        handle_export_users(vars, base_url, json_output)?;
        Ok(())
    });
    reg.register("product_analytics", |vars, base_url, json_output| {
        handle_product_analytics(vars, base_url, json_output)?;
        Ok(())
    });

    // Validate mapping vs handlers
    rclib::cli::validate_handlers(&mapping_root, &reg)?;

    // Delegate command driving to rclib
    let user_agent = format!("{}/{}", APP_NAME, env!("CARGO_PKG_VERSION"));
    let exit_code = rclib::cli::drive_command(&mapping_root, &default_base_url, &matches, &reg, &user_agent)?;
    std::process::exit(exit_code);
}

// Custom handler exanple: Export users with various formats
fn handle_export_users(
    vars: &HashMap<String, String>,
    base_url: &str,
    json_output: bool,
) -> Result<()> {
    let format = vars.get("format").map(|s| s.as_str()).unwrap_or("json");
    let output_file = vars.get("output_file").map(|s| s.as_str()).unwrap_or("users_export.json");
    let include_sensitive = vars.get("include_sensitive").map(|s| s == "true").unwrap_or(false);
    let limit = vars.get("limit").map(|s| s.as_str()).unwrap_or("100");
    let skip = vars.get("skip").map(|s| s.as_str()).unwrap_or("0");

    if json_output {
        let response = serde_json::json!({
            "operation": "export_users",
            "status": "simulated",
            "parameters": {
                "format": format,
                "output_file": output_file,
                "include_sensitive": include_sensitive,
                "limit": limit,
                "skip": skip,
                "base_url": base_url
            },
            "message": "This would export users data in real implementation"
        });
        println!("{}", serde_json::to_string_pretty(&response).unwrap());
    } else {
        println!("User Export Operation");
        println!("Format: {}", format);
        println!("Output: {}", output_file);
        println!("Sensitive data: {}", if include_sensitive { "included" } else { "excluded" });
        println!("Records: {} (starting from {})", limit, skip);
        println!("API Base: {}", base_url);
        println!("\n Export would fetch from: {}/users?limit={}&skip={}", base_url, limit, skip);
        println!(" Would save to: {}", output_file);
    }

    Ok(())
}

// Custom handler example: Product analytics and reporting
fn handle_product_analytics(
    vars: &HashMap<String, String>,
    base_url: &str,
    json_output: bool,
) -> Result<()> {
    let report_type = vars.get("report_type").map(|s| s.as_str()).unwrap_or("summary");
    let category_filter = vars.get("category_filter").map(|s| s.as_str());
    let price_range = vars.get("price_range").map(|s| s.as_str());
    let output_format = vars.get("output_format").map(|s| s.as_str()).unwrap_or("table");

    if json_output {
        let response = serde_json::json!({
            "operation": "product_analytics",
            "report_type": report_type,
            "filters": {
                "category": category_filter,
                "price_range": price_range
            },
            "output_format": output_format,
            "base_url": base_url,
            "endpoints_analyzed": [
                format!("{}/products", base_url),
                format!("{}/products/categories", base_url)
            ]
        });
        println!("{}", serde_json::to_string_pretty(&response).unwrap());
    } else {
        println!("Product Analytics Report ({})", report_type);
        if let Some(cat) = category_filter {
            println!("Category Filter: {}", cat);
        }
        if let Some(price) = price_range {
            println!("Price Range: {}", price);
        }
        println!("Output Format: {}", output_format);
        println!("\nAnalysis would include:");
        println!("  - Product count by category");
        println!("  - Price distribution analysis");
        println!("  - Rating statistics");
        println!("  - Stock level insights");
        println!("  - Brand performance metrics");
    }

    Ok(())
}
