# Makefile
.PHONY: docker-build docker-run build release clean fmt check test doc bench help tasks clippy publish publish-dry-run build-mentisdb

default: help
CARGO_CMD=/usr/bin/env cargo
# cargo-sweep (cargo install cargo-sweep), when installed, deletes whatever an older build
# left in target/ that this build didn't use, so only fresh release artifacts remain.
SWEEP := $(shell command -v cargo-sweep >/dev/null 2>&1 && echo cargo sweep)

# ----------------------------------------------------------------------------------------------------------------------
# Targets
# ----------------------------------------------------------------------------------------------------------------------

# Default target (ensures formatting before building)
build: fmt ## Build the crate in release mode (runs fmt first), then sweep stale artifacts
	$(if $(SWEEP),$(SWEEP) --stamp)
	${CARGO_CMD} build --release
	$(if $(SWEEP),$(SWEEP) --file)

# Build the mentisdb daemon binary
build-mentisdb: ## Build the mentisdb binary in release mode
	${CARGO_CMD} build --bin mentisdb --release

# Build the Docker image
docker-build: ## Build the Docker image (mentisdb:local)
	docker build -t mentisdb:local .

# Run the Docker image with a named volume, ports published on loopback only
docker-run: ## Run mentisdb:local headless with the mentisdb-data volume
	docker run --rm --name mentisdb -p 127.0.0.1:9471:9471 -p 127.0.0.1:9472:9472 -p 127.0.0.1:9475:9475 -v mentisdb-data:/var/lib/mentisdb mentisdb:local

# Full release process (ensures everything runs in the correct order)
release: fmt check clippy build test doc ## Perform a full release (fmt, check, clippy, build, test, doc)

# Format the code
fmt: ## Format the code using cargo fmt
	${CARGO_CMD} fmt

# Check for errors without building
check: ## Run cargo check --release to analyze the code without compiling
	${CARGO_CMD} check --release
	${CARGO_CMD} check --release --bin mentisdb

# Strict linter, fails on warnings and suggests fixes
clippy: ## Run clippy and fail on warnings
	${CARGO_CMD} fmt
	${CARGO_CMD} clippy --release --all-targets --all-features -- -D warnings

# Run tests
test: ## Run all tests using cargo test --release
	${CARGO_CMD} test --release

# Run benchmarks
bench: ## Run Criterion benchmarks (thought_chain, skill_registry, http_concurrency)
	MENTISDB_THOUGHT_SOUNDS=false ${CARGO_CMD} bench 2>&1 | tee /tmp/mentisdb_bench_results.txt

# Generate documentation
doc: ## Generate project documentation using cargo doc --release
	${CARGO_CMD} doc --release --all-features

# Publish to crates.io
publish: ## Publish mentisdb to crates.io
	${CARGO_CMD} publish

# Dry-run publish to crates.io
publish-dry-run: ## Dry-run publish to crates.io
	${CARGO_CMD} publish --dry-run

# Install the mentisdb executable locally (local-embeddings is a default feature)
install: ## Install mentisdb via cargo install
	${CARGO_CMD} install --path . --locked

# Clean build artifacts
clean: ## Remove build artifacts using cargo clean
	${CARGO_CMD} clean

# Show all available tasks
help tasks: ## Show this help message
	@echo "Available commands:"
	@grep -E '^[a-zA-Z_-]+:.*##' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'
