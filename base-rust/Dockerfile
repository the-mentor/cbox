# cbox-base plus what building cbox itself needs: a Rust toolchain and protoc
# (boxlite's build.rs requires protoc >= 3.12). Build custom/ on it with
# CBOX_BASE_IMAGE=ghcr.io/the-mentor/cbox-base-rust:latest just build.
ARG BASE_IMAGE=ghcr.io/the-mentor/cbox-base:latest
FROM ${BASE_IMAGE}

USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config protobuf-compiler rustup \
    && rm -rf /var/lib/apt/lists/*

# base/ links /usr/local/bin/cc to Claude Code as a shorthand, shadowing the C
# compiler rustc links through and the cc crate compiles with; use gcc by name.
ENV CC=gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=gcc

# Installed as sandbox-user into its own ~/.rustup and ~/.cargo (rustup's
# defaults), so cargo and rustup never need sudo. apt's rustup puts the
# cargo/rustc proxies in /usr/bin; ~/.cargo/bin is for `cargo install`.
# Same pinned version as ci.yml's RUST_VERSION, so the box builds cbox with the
# release compiler; Renovate bumps both in one PR.
USER sandbox-user
ENV PATH=/home/sandbox-user/.cargo/bin:$PATH
# renovate: datasource=github-releases depName=rust-lang/rust
ARG RUST_VERSION=1.99.1
RUN rustup toolchain install "$RUST_VERSION" --profile minimal -c rustfmt -c clippy \
    && rustup default "$RUST_VERSION"
