# RHAF Portal Core

Rust runtime for RHAF Portal. The runtime builds on the [Wanaku](https://github.com/wanaku-ai/wanaku) project.
The operator and portal UI belong in separate repositories.

## Core Guidelines

Think before you write. Do not create abstractions unnecessarily. Simplicity
matters: use the minimum code required to achieve the result. Do not bypass Rust
or Clippy rules.

Use Rust for tests and workspace checks. Do not add Python code or runtime requirements.

## Quick Start

```bash
cargo build --workspace --locked
cargo run --locked
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

## Build Info

- Rust 2024; MSRV 1.96; toolchain pinned to 1.96.0.
- Workspace Rust and Clippy lints build on Wanaku. Thresholds are in `clippy.toml`.
- Unsafe code, `unwrap`, `expect`, `panic`, `todo!`, and `unimplemented!` are denied.
- Every workspace crate must set `[lints] workspace = true` in its manifest.
- Run `cargo check-workspace-lints` to check inheritance.
- Wanaku dependencies use one pinned Git revision. The upstream admin UI is disabled.

## Repository Layout

- `src/main.rs`: embedded Wanaku executable.
- `src/startup.rs`: CLI, bootstrap validation, and persistence resources.
- `src/runtime.rs`: listeners, background services, and orderly shutdown.
- `src/pipeline.yaml`: internal MCP pipeline.
- `tests/runtime_integration.rs`: real MCP fixture and process integration checks.
- `xtask/`: container build and manifest publication commands.
- `.cargo/config.toml`: Cargo aliases for the build task runner.
- `Containerfile`: UBI 10.1 builder and UBI 10.1 Minimal runtime.
- `.github/workflows/`: PR, main, link, early-access, and release workflows.
- `jreleaser.yml` and `jreleaser/`: release metadata and changelog template.
- `.oss-ai-helper-rules/`: project metadata and contribution rules.

## Containers and Releases

```bash
cargo build-image --tag localhost/rhaf-portal-core:dev
cargo build-image --push --tag REGISTRY/GROUP/rhaf-portal-core:TAG
cargo build-image-manifest --tag REGISTRY/GROUP/rhaf-portal-core:TAG
```

Build tasks use Docker, with Podman as a fallback when Docker is unusable.
Image tags have a native `-x86_64` or `-aarch64` suffix. Manifest publication
requires Podman and both architecture images in the registry. CI builds each
architecture on a native Linux runner. The runtime image uses UID 1001 and
GID 0 for OpenShift support.

Keep the Containerfile source-copy and cache-build stages synchronized when you
add crates, build scripts, or source directories. Keep Rust versions synchronized
across Cargo metadata, `rust-toolchain.toml`, `clippy.toml`, Containerfile, and CI.

Release workflows package Linux and macOS binaries for x86_64 and ARM64. JReleaser
signs archives and creates GitHub releases. See README.md for required variables,
secrets, and release version rules.

## Style and Testing

- Use `#[expect(..., reason = "...")]` only for justified lint exceptions.
- Group related arguments into context structs instead of suppressing argument limits.
- Add meaningful tests and documentation for new features.
- Run the process integration check with the controlled MCP fixture. See README.md.
- Keep similar changes in a single commit before opening a pull request.
- Use `git mv` to move tracked files.
- Prefer signed commits; unsigned commits are permitted if signing fails.
- Use an appropriate specialized agent when available.
- Use OSS Helper and applicable Ripwire skills for code work.

## Documentation

Use ASD-STE100 Simplified Technical English for procedures, commands,
configuration, rules, and operational behavior. Use natural technical English
for introductions and other reader-facing content.

Use short, direct sentences and active voice. Put one instruction in each
sentence. Keep terminology consistent. Preserve commands, URLs, identifiers,
product names, and technical meaning when editing documentation.

Documentation must be written in goal-oriented style. When writing documentation, always consider the audience.
