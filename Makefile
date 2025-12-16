# rclib Makefile
# Thorough code checking and development automation

CI := 1

# Show the help message with list of commands (default target)
help:
	@echo "rclib Development Commands"
	@echo "=========================="
	@echo ""
	@echo "Code Formatting:"
	@echo "  make fmt          - Check code formatting"
	@echo "  make dev-fmt      - Auto-fix code formatting"
	@echo ""
	@echo "Code Quality:"
	@echo "  make clippy       - Run clippy linter"
	@echo "  make lint         - Check for compile warnings"
	@echo "  make dev-clippy   - Auto-fix clippy warnings"
	@echo ""
	@echo "Code Safety:"
	@echo "  make kani         - Run Kani verifier for safety checks"
	@echo "  make geiger       - Run Geiger scanner for unsafe code"
	@echo "  make safety       - Run all code safety checks"
	@echo ""
	@echo "Security:"
	@echo "  make deny         - Check licenses and dependencies"
	@echo "  make security     - Run all security checks"
	@echo ""
	@echo "Tests:"
	@echo "  make test         - Run all tests"
	@echo "  make test-lib     - Run library tests only"
	@echo "  make test-int     - Run integration tests only"
	@echo ""
	@echo "Coverage:"
	@echo "  make coverage     - Generate code coverage report (HTML)"
	@echo "  make coverage-text - Generate code coverage report (text)"
	@echo ""
	@echo "Development:"
	@echo "  make dev          - Auto-fix formatting and clippy, then test"
	@echo "  make dev-test     - Run tests in development mode"
	@echo ""
	@echo "Build:"
	@echo "  make build        - Make a release build"
	@echo "  make run          - Run the dummyjson-cli example"
	@echo ""
	@echo "Main Targets:"
	@echo "  make check        - Run all quality checks"
	@echo "  make ci           - Run CI pipeline"
	@echo "  make all          - Run all checks, tests, and build"

# -------- Code formatting --------
.PHONY: fmt

# Check code formatting
fmt:
	cargo fmt --all -- --check

# -------- Code quality --------
.PHONY: clippy lint

# Run clippy linter
clippy:
	cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::perf

# Check there are no compile time warnings
lint:
	RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --all-features

# -------- Code safety checks --------
.PHONY: kani geiger safety

# The Kani Rust Verifier for checking safety of the code
kani:
	@command -v kani >/dev/null || \
		(echo "Installing Kani verifier..." && \
		 cargo install --locked kani-verifier)
	cargo kani --workspace --all-features

# Run Geiger scanner for unsafe code in dependencies
geiger:
	cargo geiger --all-features

# Run all code safety checks
safety: clippy lint
	@echo "OK. Rust Safety Pipeline complete"

# -------- Code security checks --------
.PHONY: deny security

# Check licenses and dependencies
deny:
	cargo deny check

# Run all security checks
security: deny
	@echo "OK. Rust Security Pipeline complete"

# -------- Development and auto fix --------
.PHONY: dev dev-fmt dev-clippy dev-test

# Run tests in development mode
dev-test:
	cargo test --workspace

# Auto-fix code formatting
dev-fmt:
	cargo fmt --all

# Auto-fix clippy warnings
dev-clippy:
	cargo clippy --workspace --all-targets --fix --allow-dirty

# Auto-fix formatting and clippy warnings
dev: dev-fmt dev-clippy dev-test

# -------- Tests --------
.PHONY: test test-lib test-int

# Run all tests
test:
	cargo test --workspace

# Run library tests only
test-lib:
	cargo test -p rclib --lib

# Run integration tests only
test-int:
	cargo test -p rclib --test integration_tests

# -------- Code coverage --------
.PHONY: coverage coverage-text

# Generate code coverage report (HTML)
coverage:
	@command -v cargo-llvm-cov >/dev/null || (echo "Installing cargo-llvm-cov..." && cargo install cargo-llvm-cov)
	cargo llvm-cov --workspace --html
	@echo "Coverage report generated at target/llvm-cov/html/index.html"

# Generate code coverage report (text)
coverage-text:
	@command -v cargo-llvm-cov >/dev/null || (echo "Installing cargo-llvm-cov..." && cargo install cargo-llvm-cov)
	cargo llvm-cov --workspace

# -------- Build --------
.PHONY: build run

# Make a release build using stable toolchain
build:
	cargo +stable build --release

# Run the dummyjson-cli example
run:
	cargo run -p dummyjson-cli -- --help

# -------- Main targets --------
.PHONY: check ci all

# Run all quality checks
check: fmt clippy lint test security

# Run CI pipeline
ci: check

# Run all necessary quality checks and tests and then build the release binary
all: check build
	@echo "All checks passed and release binary built successfully"
