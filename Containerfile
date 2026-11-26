# syntax=docker/dockerfile:1

# Rust toolchain and build dependencies.
FROM registry.access.redhat.com/ubi10/ubi:10.1 AS rust-base

RUN dnf install -y gcc gcc-c++ git openssl-devel pkgconf-pkg-config cmake make \
    && dnf clean all
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.96.0
ENV PATH="/root/.cargo/bin:${PATH}"

FROM rust-base AS builder
WORKDIR /src

# Cache dependencies before copying application sources.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY vendor/wanaku vendor/wanaku
COPY xtask/Cargo.toml xtask/Cargo.toml
# The contract crate generates OpenAPI during compilation.
COPY crates/rhaf-api crates/rhaf-api
RUN mkdir -p src xtask/src \
    && printf 'fn main() {}\n' > src/main.rs \
    && printf 'fn main() {}\n' > xtask/src/main.rs
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p rhaf-portal-core -p rhaf-api

COPY src src
# Ensure Cargo rebuilds the real sources after the cache build.
RUN touch src/main.rs
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p rhaf-portal-core -p rhaf-api \
    && cp target/release/rhaf-portal-core /usr/local/bin/rhaf-portal-core

FROM registry.access.redhat.com/ubi10/ubi-minimal:10.1

LABEL org.opencontainers.image.source="https://github.com/rhaf-ai/rhaf-portal-core" \
    org.opencontainers.image.description="RHAF Portal Core" \
    org.opencontainers.image.licenses="Apache-2.0"

RUN microdnf install -y ca-certificates openssl-libs \
    && microdnf clean all \
    && mkdir -p /etc/rhaf-portal \
    && chown 0:0 /etc/rhaf-portal \
    && chmod g=u /etc/rhaf-portal

COPY --from=builder --chown=root:root --chmod=0555 \
    /usr/local/bin/rhaf-portal-core /usr/local/bin/rhaf-portal-core

# OpenShift can override the UID; GID 0 retains access to the working directory.
USER 1001:0
WORKDIR /etc/rhaf-portal
ENTRYPOINT ["rhaf-portal-core"]
