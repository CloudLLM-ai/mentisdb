# syntax=docker/dockerfile:1.7

# ---- builder ----------------------------------------------------------------
# Debian trixie (glibc 2.41, GCC 14) rather than bookworm (glibc 2.36): the
# prebuilt ONNX Runtime that ort/fastembed 5.x link for local-embeddings is
# compiled against glibc >= 2.38 / GCC 13 libstdc++ and fails to link on
# bookworm (`undefined reference to __isoc23_strtol`, `_M_replace_cold`).
# The runtime stage must match the builder's glibc. The image is self-contained,
# so the host's glibc does not matter.
# CI (build.yml / test.yml) uses the `stable` toolchain and Cargo.toml declares
# no `rust-version`, so track the current stable 1.x line. Cargo.lock pins every
# crate, so image builds stay reproducible with `--locked`. Several locked
# crates need rustc >= 1.88 and edition 2024 support.
ARG RUST_VERSION=1
FROM rust:${RUST_VERSION}-trixie AS builder

# Features compiled into the image. Mirrors the crate defaults
# (server + llm-extraction + local-embeddings) minus `startup-sound`, which pulls
# in rodio/alsa and is meaningless in a headless container.
ARG CARGO_FEATURES="server,llm-extraction,local-embeddings"

# pkg-config + libssl-dev: fastembed's hf-hub downloader links OpenSSL.
# fastembed/ort downloads a prebuilt static ONNX Runtime during the build
# (needs network + ca-certificates).
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config \
      libssl-dev \
      ca-certificates \
      cmake \
      clang \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# The only binary is `mentisdb` (src/bin/mentisdb.rs, feature `server`).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --no-default-features \
      --features "${CARGO_FEATURES}" --bin mentisdb \
    && install -D target/release/mentisdb /out/mentisdb

# ---- runtime ----------------------------------------------------------------
FROM debian:trixie-slim AS runtime

# libssl3t64       dynamic OpenSSL used by fastembed's model downloader
# libstdc++6/libgomp1  runtime deps of the bundled ONNX Runtime
# ca-certificates TLS roots (model download, update checks)
# curl            HEALTHCHECK probe
# tini            PID 1: reaps zombies and forwards SIGTERM
RUN apt-get update && apt-get install -y --no-install-recommends \
      libssl3t64 \
      libstdc++6 \
      libgomp1 \
      ca-certificates \
      curl \
      tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 991 mentisdb \
    && useradd --system --uid 991 --gid 991 --home-dir /var/lib/mentisdb \
              --create-home --shell /usr/sbin/nologin mentisdb

COPY --from=builder /out/mentisdb /usr/local/bin/mentisdb

# Build-time sanity checks: every shared library resolves and the binary runs.
RUN ldd /usr/local/bin/mentisdb && ! ldd /usr/local/bin/mentisdb | grep -q "not found" \
    && /usr/local/bin/mentisdb --help >/dev/null

# 0.0.0.0 is required for `docker run -p` to reach the daemon, so keep the
# container private by publishing ports on 127.0.0.1 only (see README) and turn
# on MENTISDB_BEARER_TOKEN_ACCESS / MENTISDB_DASHBOARD_PIN before exposing it.
# The fastembed model cache defaults to $MENTISDB_DIR/.fastembed_cache, so it
# lives in the volume and is downloaded once.
ENV MENTISDB_DIR=/var/lib/mentisdb \
    MENTISDB_BIND_HOST=0.0.0.0 \
    MENTISDB_MCP_PORT=9471 \
    MENTISDB_REST_PORT=9472 \
    MENTISDB_HTTPS_MCP_PORT=0 \
    MENTISDB_HTTPS_REST_PORT=0 \
    MENTISDB_DASHBOARD_PORT=9475 \
    MENTISDB_STARTUP_SOUND=false \
    MENTISDB_THOUGHT_SOUNDS=false \
    MENTISDB_UPDATE_CHECK=false \
    RUST_LOG=info

USER mentisdb
WORKDIR /var/lib/mentisdb

VOLUME ["/var/lib/mentisdb"]
# 9471 MCP (HTTP), 9472 REST (HTTP), 9475 dashboard (HTTPS, self-signed).
# HTTPS MCP/REST (9473/9474) are disabled by default; set
# MENTISDB_HTTPS_MCP_PORT / MENTISDB_HTTPS_REST_PORT to enable them.
EXPOSE 9471 9472 9475

# /health on the REST port is unauthenticated and cheap.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD curl -fsS "http://127.0.0.1:${MENTISDB_REST_PORT}/health" >/dev/null || exit 1

# `--headless` skips the TUI (no TTY / `-t` needed). tini forwards SIGTERM and
# the daemon drains its servers and exits 0.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/mentisdb", "--headless"]
