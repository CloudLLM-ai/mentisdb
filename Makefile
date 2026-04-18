# Makefile
.PHONY: build release clean fmt check test doc bench help tasks clippy publish publish-dry-run build-mentisdbd

default: help
CARGO_CMD=/usr/bin/env cargo

# ----------------------------------------------------------------------------------------------------------------------
# Targets
# ----------------------------------------------------------------------------------------------------------------------

# Default target (ensures formatting before building)
build: fmt ## Build the crate in release mode (runs fmt first)
	${CARGO_CMD} build --release

# Build the mentisdbd daemon binary
build-mentisdbd: ## Build the mentisdbd binary in release mode
	${CARGO_CMD} build --bin mentisdbd --release

# Full release process (ensures everything runs in the correct order)
release: fmt check clippy build test doc ## Perform a full release (fmt, check, clippy, build, test, doc)

# Format the code
fmt: ## Format the code using cargo fmt
	${CARGO_CMD} fmt

# Check for errors without building
check: ## Run cargo check to analyze the code without compiling
	${CARGO_CMD} check
	${CARGO_CMD} check --bin mentisdbd

# Strict linter, fails on warnings and suggests fixes
clippy: ## Run clippy and fail on warnings
	${CARGO_CMD} fmt
	${CARGO_CMD} clippy --all-targets --all-features -- -D warnings

# Run tests
test: ## Run all tests using cargo test
	${CARGO_CMD} test

# Run benchmarks
bench: ## Run Criterion benchmarks (thought_chain, skill_registry, http_concurrency)
	MENTISDB_THOUGHT_SOUNDS=false ${CARGO_CMD} bench 2>&1 | tee /tmp/mentisdb_bench_results.txt

# Generate documentation
doc: ## Generate project documentation using cargo doc
	${CARGO_CMD} doc --all-features

# Publish to crates.io
publish: ## Publish mentisdb to crates.io
	${CARGO_CMD} publish

# Dry-run publish to crates.io
publish-dry-run: ## Dry-run publish to crates.io
	${CARGO_CMD} publish --dry-run

# Install the mentisdbd executable locally
install: ## Install mentisdbd via cargo install
	${CARGO_CMD} install --path . --locked

# Clean build artifacts
clean: ## Remove build artifacts using cargo clean
	${CARGO_CMD} clean

# Show all available tasks
help tasks: ## Show this help message
	@echo "Available commands:"
	@grep -E '^[a-zA-Z_-]+:.*##' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'
