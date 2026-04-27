# syntax=docker/dockerfile:1.7

# ---- builder ----------------------------------------------------------------
FROM rust:1.83-bookworm AS builder

# System deps required to compile alsa-sys + reqwest TLS (openssl-sys).
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config \
      libssl-dev \
      libasound2-dev \
      ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# `cargo install --path` builds the `mentisdbd` bin in release mode and drops
# the artefact into /usr/local/cargo/bin/. `--locked` enforces Cargo.lock so
# image builds are reproducible across mentisdb releases.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo install --path . --locked --bin mentisdbd --root /opt/mentisdb

# ---- runtime ----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Runtime deps: libasound2 (alsa-sys link), libssl3 (rustls fallback for some
# crates), ca-certificates (TLS roots), tini (signal-forwarding init).
RUN apt-get update && apt-get install -y --no-install-recommends \
      libasound2 \
      libssl3 \
      ca-certificates \
      tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 991 mentisdb \
    && useradd --system --uid 991 --gid 991 --home-dir /var/lib/mentisdb \
              --create-home --shell /usr/sbin/nologin mentisdb

COPY --from=builder /opt/mentisdb/bin/mentisdbd /usr/local/bin/mentisdbd

# Default data dir + ports. Override via env / mounts at run time.
ENV MENTISDB_DIR=/var/lib/mentisdb \
    MENTISDB_BIND_HOST=0.0.0.0 \
    MENTISDB_MCP_PORT=9471 \
    MENTISDB_REST_PORT=9472 \
    MENTISDB_HTTPS_MCP_PORT=0 \
    MENTISDB_HTTPS_REST_PORT=0 \
    MENTISDB_DASHBOARD_PORT=9475 \
    MENTISDB_STARTUP_SOUND=false \
    MENTISDB_THOUGHT_SOUNDS=false \
    RUST_LOG=info

USER mentisdb
WORKDIR /var/lib/mentisdb

VOLUME ["/var/lib/mentisdb"]
EXPOSE 9471 9472 9475

# tini reaps zombies + forwards SIGTERM cleanly to mentisdbd.
# Run with `docker run -dt ...` (-t allocates a PTY that mentisdbd's /dev/tty
# probe needs; without it the daemon exits on first boot. Track upstream for
# a `--headless` flag that removes this requirement.)
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/mentisdbd"]
