# dummyjson-cli - Example CLI for DummyJSON Mock API

A comprehensive command-line interface for the [DummyJSON](https://dummyjson.com) mock API server. This example demonstrates advanced `rclib` features including custom handlers, scenarios, batch operations, and performance testing.

## What This Example Demonstrates

This CLI showcases **every major rclib feature** through a practical e-commerce/mock data API:

- ✅ **Complete CRUD Operations** - GET, POST, PUT, DELETE across all endpoints
- ✅ **Advanced Argument Handling** - Inheritance, overrides, boolean flags, positional args
- ✅ **Custom Handlers** - Complex business logic implemented in Rust
- ✅ **Multi-step Scenarios** - Job scheduling with polling simulation
- ✅ **File Content Override** - JSON file replacement for complex payloads
- ✅ **Table Views** - Rich formatting with size modifiers and nested object access
- ✅ **Performance Testing** - Built-in load testing and stress testing
- ✅ **Batch Operations** - Data export, analytics, and reporting

## Overview

This approach demonstrates how `rclib` enables rapid CLI development with minimal Rust code.

## Architecture

### Core Files

1. **main.rs**: CLI entry point with 2 custom handlers (`export_users`, `product_analytics`) demonstrating different patterns
2. **mapping.yaml**: Comprehensive command definitions showcasing every rclib feature
3. **dummyjson-openapi-spec.yaml**: DummyJSON API schema for e-commerce mock data

### Key Features Demonstrated

- **9 Command Categories**: Products, Users, Carts, Todos, Posts, Auth, Scenario, Batch, Performance
- **Custom Handler Patterns**: User data export utilities and product analytics engines
- **Advanced Argument Patterns**: Multi-level inheritance, conditional overrides, file content replacement
- **Rich Output Formatting**: Table views with size modifiers (`:mb`) and nested object access
- **Performance Testing**: Built-in load testing with concurrency control and detailed statistics
- **Multi-step Scenarios**: Job polling simulation with timeout and error handling

## Build & Run

```bash
# From the project root
cargo build
./target/debug/dummyjson-cli --help
```

## Usage Examples

### Basic Operations
```bash
# Product catalog browsing
dummyjson-cli products list --limit 10 --select "title,price,category"
dummyjson-cli products search --query "phone" --limit 5
dummyjson-cli products by-category --category smartphones
dummyjson-cli products get 1

# User management
dummyjson-cli users list --limit 5
dummyjson-cli users search --query "john"
dummyjson-cli users filter --field age --value 25
dummyjson-cli users get 1

# Shopping cart operations
dummyjson-cli carts list
dummyjson-cli carts user-carts --user-id 1
dummyjson-cli -j carts user-carts --user-id 1 # same, but json output
dummyjson-cli carts add --user-id 1 --products '[{"id": 1, "quantity": 2}]'

# Todo management with boolean flags
dummyjson-cli todos list
dummyjson-cli todos add --todo "Learn rclib" --user-id 1 --completed
dummyjson-cli todos update --id 1 --completed
dummyjson-cli todos delete 1
```

### Authentication
```bash
# Login and get access token
dummyjson-cli auth login --username kminchelle --password 0lelplR

# Use token for authenticated requests
dummyjson-cli auth me --token YOUR_TOKEN_HERE
```

### File Override Features
```bash
# Create cart with JSON file
echo '[{"id": 1, "quantity": 3}, {"id": 2, "quantity": 1}]' > cart.json
dummyjson-cli carts add --user-id 1 --products-file cart.json

# Create post with content from file
echo "This is my blog post content..." > post.txt
dummyjson-cli posts add --title "My Post" --user-id 1 --content-file post.txt
```

### Custom Handlers (Advanced Features)
```bash
# Data export with various formats
dummyjson-cli batch export-users --format csv --output users.csv --include-sensitive # same, but json output
dummyjson-cli -j batch export-users --format csv --output users.csv --include-sensitive # same, but json output
dummyjson-cli batch export-users --format json --limit 50 --skip 10

# Product analytics and reporting
dummyjson-cli batch product-analytics --report-type detailed --category smartphones
dummyjson-cli batch product-analytics --price-range "100-500" --output-format table

# Performance testing
dummyjson-cli --count 50 --concurrency 5 performance load-test --delay 100

```

### Global Options
```bash
# All commands support these global options
dummyjson-cli --help                                    # Show help
dummyjson-cli --base-url https://dummyjson.com         # Set API base URL
dummyjson-cli --json-output                            # Output in JSON format
dummyjson-cli --verbose                                # Verbose HTTP output
dummyjson-cli --timeout 60                             # Request timeout in seconds
dummyjson-cli --conn-timeout 10                        # Connection timeout in seconds

# Performance testing options (work with any command)
dummyjson-cli products list --count 100 --concurrency 10    # Repeat request 100 times with 10 concurrent
dummyjson-cli users list --duration 30 --concurrency 5      # Run for 30 seconds with 5 concurrent requests
```

## Feature Implementation Status

This CLI demonstrates comprehensive rclib feature coverage through DummyJSON API endpoints.

### Core CRUD Operations

**Products Management (`products`)**
- [x] `list`: Paginated product catalog with table view
- [x] `get`: Individual product details by ID
- [x] `search`: Text search with query parameters
- [x] `categories`: List all available categories
- [x] `by-category`: Filter products by category

**Users Management (`users`)**
- [x] `list`: User directory with filtering
- [x] `get`: User profile details
- [x] `search`: Find users by query
- [x] `filter`: Advanced filtering by field/value pairs

**Shopping Carts (`carts`)**
- [x] `list`: All shopping carts with size modifiers (`:mb`)
- [x] `get`: Individual cart details
- [x] `user-carts`: Carts for specific user
- [x] `add`: Create cart with JSON file override support

**Todo Management (`todos`)**
- [x] `list`: Todo list with completion filtering
- [x] `get`: Individual todo details
- [x] `user-todos`: User-specific todos
- [x] `add`: Create todos with boolean flags
- [x] `update`: Update completion status
- [x] `delete`: Remove todos

**Posts & Content (`posts`)**
- [x] `list`: Blog posts with pagination
- [x] `get`: Individual post details
- [x] `search`: Content search
- [x] `user-posts`: Posts by specific author
- [x] `add`: Create posts with file content override

### ✅ Authentication & Security

**Authentication (`auth`)**
- [x] `login`: Username/password authentication
- [x] `me`: Get authenticated user profile
- [x] `refresh`: Token refresh mechanism

### ✅ Advanced rclib Features

**Multi-step Scenarios (`scenario`)**
- [x] `user-shopping-summary`: Job scheduling with polling simulation

**Batch Operations (`batch`)**
- [x] `export-users`: Data export with format selection (CSV, JSON, XML)
- [x] `product-analytics`: Advanced reporting and analytics engine

**Performance Testing (`performance`)**
- [x] `load-test`: Built-in load testing with delay simulation

### ✅ rclib Feature Matrix

| Feature | Implementation | Example Commands |
|---------|---------------|------------------|
| **GET Operations** | ✅ Complete | `products list`, `users get 1` |
| **POST Operations** | ✅ Complete | `auth login`, `carts add` |
| **PUT/DELETE** | ✅ Complete | `todos update`, `todos delete` |
| **Query Parameters** | ✅ Complete | `--limit`, `--skip`, `--select` |
| **Path Parameters** | ✅ Complete | `/products/{id}`, `/users/{id}` |
| **Request Bodies** | ✅ Complete | JSON templates with variables |
| **Table Views** | ✅ Complete | Formatted lists with column selection |
| **Boolean Flags** | ✅ Complete | `--completed`, `--include-sensitive` |
| **File Override** | ✅ Complete | `--products-file`, `--content-file` |
| **Common Args** | ✅ Complete | Inherited pagination/filtering |
| **Group Args** | ✅ Complete | Category-specific arguments |
| **Custom Handlers** | ✅ Complete | 2 handlers: export_users, product_analytics |
| **Scenarios** | ✅ Complete | Multi-step with polling |
| **Positional Args** | ✅ Complete | `dummyjson-cli users get 1` |
| **Short Flags** | ✅ Complete | `-q`, `-u`, `-p` |
| **Size Modifiers** | ✅ Complete | `:mb` for byte conversion |
| **Nested Access** | ✅ Complete | `user.login` in table views |

### 🎯 Unique Demonstrations

This example uniquely showcases:
- **E-commerce Workflows**: Complete shopping cart and product management
- **Multi-format Export**: Data export with CSV/JSON/XML options
- **Analytics Engine**: Product performance analysis and reporting
- **Load Testing**: Built-in performance testing with configurable delays
- **File-based Configuration**: JSON payload management through files
- **Scenario Operations**: Multi-step workflows with job polling simulation

## Learning rclib

This example serves as a comprehensive tutorial for rclib features:

1. **Start Simple**: Try basic CRUD operations (`products list`, `users get 1`)
2. **Explore Filtering**: Use search and filtering features (`products search --query "phone"`)
3. **Try File Overrides**: Create complex payloads with JSON files
4. **Test Performance**: Use built-in load testing with `--count` and `--concurrency`
5. **Custom Logic**: Examine the custom handlers in `main.rs`
6. **Advanced Features**: Try scenarios and batch operations

The mapping.yaml file serves as a comprehensive reference for rclib features, while the 2 custom handlers in main.rs (`export_users` and `product_analytics`) demonstrate how to implement complex business logic when declarative YAML isn't sufficient.
