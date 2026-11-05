# cbox-base plus what building cbox itself needs: a Rust toolchain and protoc
# (boxlite's build.rs requires protoc >= 3.12). Build custom/ on it with
# CBOX_BASE_IMAGE=ghcr.io/the-mentor/cbox-base-rust:latest just build.
ARG BASE_IMAGE=ghcr.io/the-mentor/cbox-base:latest
FROM ${BASE_IMAGE}

USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config protobuf-compiler rustup \
    && rm -rf /var/lib/apt/lists/*

# Same pinned version as ci.yml's RUST_VERSION, so the box builds cbox with the
# release compiler; Renovate bumps both in one PR. Shared homes owned by
# sandbox-user so cargo's registry cache and `rustup component add` work
# without sudo.
# renovate: datasource=github-releases depName=rust-lang/rust
ARG RUST_VERSION=1.98.1
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# base/ links /usr/local/bin/cc to Claude Code as a shorthand, shadowing the C
# compiler rustc links through and the cc crate compiles with; use gcc by name.
ENV CC=gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=gcc

RUN rustup toolchain install "$RUST_VERSION" --profile minimal -c rustfmt -c clippy \
    && rustup default "$RUST_VERSION" \
    && mkdir -p "$CARGO_HOME" \
    && chown -R sandbox-user:sandbox-user "$RUSTUP_HOME" "$CARGO_HOME"

USER sandbox-user
