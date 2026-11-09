# Makefile
.PHONY: coverage coverage-open docker-build docker-run build release clean fmt check test doc bench help tasks clippy publish publish-dry-run build-mentisdb

default: help
CARGO_CMD=/usr/bin/env cargo
# cargo-sweep (cargo install cargo-sweep), when installed, deletes whatever an older build
# left in target/ that this build didn't use, so only fresh release artifacts remain.
SWEEP := $(shell command -v cargo-sweep >/dev/null 2>&1 && echo cargo sweep)

# Coverage (docs/ENGINEERING.md, "Coverage"). One run on a pinned nightly measures functions,
# branches, regions and lines (branch coverage is nightly-only). No lcov export: llvm-cov's lcov
# exporter crashes (SIGSEGV in CoverageExporterLcov) on branch data with this nightly. Keep the nightly in sync with
# .github/workflows/coverage.yml. COVERAGE_GOAL is the target; the COVERAGE_FLOOR_* values are
# what the gate enforces: the measured value rounded down. Floors only go up, never down.
COVERAGE_NIGHTLY=nightly-2026-10-01
COVERAGE_GOAL=90
COVERAGE_FLOOR_FUNCTIONS=73
COVERAGE_FLOOR_BRANCHES=64
COVERAGE_FLOOR_REGIONS=72
COVERAGE_FLOOR_LINES=74
COVERAGE_DIR=target/coverage

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

# Tests under instrumentation once, then every report from the same profile data (needs
# cargo install cargo-llvm-cov and `rustup toolchain install $(COVERAGE_NIGHTLY) --component llvm-tools-preview`).
coverage: ## Coverage table (functions, branches, regions, lines) + HTML + JSON; fails under the floors
	mkdir -p $(COVERAGE_DIR)
	${CARGO_CMD} +$(COVERAGE_NIGHTLY) llvm-cov clean --workspace
	${CARGO_CMD} +$(COVERAGE_NIGHTLY) llvm-cov --no-report --branch --all-features --locked
	${CARGO_CMD} +$(COVERAGE_NIGHTLY) llvm-cov report --branch --html --output-dir $(COVERAGE_DIR)
	${CARGO_CMD} +$(COVERAGE_NIGHTLY) llvm-cov report --branch --json --summary-only --output-path $(COVERAGE_DIR)/summary.json
	python3 scripts/coverage_gate.py $(COVERAGE_DIR)/summary.json --goal $(COVERAGE_GOAL) \
		--floor functions=$(COVERAGE_FLOOR_FUNCTIONS) --floor branches=$(COVERAGE_FLOOR_BRANCHES) \
		--floor regions=$(COVERAGE_FLOOR_REGIONS) --floor lines=$(COVERAGE_FLOOR_LINES) \
		--toolchain "$(COVERAGE_NIGHTLY)" --markdown $(COVERAGE_DIR)/summary.md

coverage-open: coverage ## Open the HTML coverage report (macOS)
	open $(COVERAGE_DIR)/html/index.html

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
