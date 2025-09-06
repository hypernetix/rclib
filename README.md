# rclib - Rust CLI Builder Library

A powerful Rust library for building command-line interfaces for REST APIs using declarative YAML configuration combined with custom imperative handlers.

## What is rclib?

`rclib` (Rust CLI Library) transforms YAML mapping files and OpenAPI specifications into fully functional CLI applications. It bridges the gap between declarative API definitions and interactive command-line tools, allowing developers to quickly create sophisticated CLIs without writing boilerplate argument parsing and HTTP request code.

## Project Structure

This repository contains:

- **`rclib/`** - The core library crate that provides CLI building functionality
- **`dummyjson-cli/`** - Example CLI application for DummyJSON API (demonstrates different feature combinations)

## Key Concepts

### Architecture Overview

```
OpenAPI Spec ──┐
               ├── rclib ──> CLI Binary ──> REST API Server
mapping.yaml ──┘
```

- **OpenAPI Spec**: Provides API schema, base URLs, and endpoint definitions
- **mapping.yaml**: Defines CLI command structure, arguments, and API mappings
- **rclib**: Core library handling HTTP requests, argument processing, and command orchestration
- **CLI Binary**: Your application built with rclib

### Core Features

- **Declarative Command Structure**: Define commands in YAML, not Rust code
- **Three Command Types**: Single API calls, multi-step scenarios, and custom handlers
- **Advanced Argument Handling**: Inheritance, overrides, boolean flags, file content replacement
- **Rich Output Formatting**: Table views with column selection and data transformation
- **Performance Testing**: Built-in load testing with concurrency and statistics
- **Template Substitution**: Variable replacement in URLs, headers, and request bodies

## Quick Start

See the example applications:
- [`dummyjson-cli/README.md`](dummyjson-cli/README.md) - E-commerce mock API CLI with products, users, and analytics

For detailed library documentation, see [`rclib/README.md`](rclib/README.md).

## Use Cases

- **API Testing Tools**: Quickly build CLIs for testing REST APIs
- **DevOps Automation**: Create command-line interfaces for internal services
- **Microservice Management**: Build unified CLIs for distributed systems
- **Mock API Clients**: Rapid prototyping of API client interfaces
- **Performance Testing**: Load testing with built-in concurrency and reporting
