# RHAF Portal Core

RHAF Portal Core is the Rust runtime for RHAF Portal. The runtime builds on the [Wanaku](https://github.com/wanaku-ai/wanaku) project.

## Build and run

Rust 1.96.0 is pinned in `rust-toolchain.toml`. Install Rust through rustup, then run:

```bash
cargo build --workspace --locked
cargo run --locked
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Install a C/C++ compiler, CMake, Make, pkg-config, Git, and OpenSSL development
libraries before the first build. The workspace builds on Wanaku's Rust and
Clippy settings. It also denies `todo!()` and `unimplemented!()` to reject
unfinished code.

Every workspace crate must inherit the shared lint settings. Add this to each
crate's `Cargo.toml`:

```toml
[lints]
workspace = true
```

PR CI checks this requirement. Run the same Rust check locally:

```bash
cargo check-workspace-lints
```

The `rhaf-api` crate generates RHAF OpenAPI during compilation. See
[API contracts](docs/api-contracts.md) for schema snapshots, generation commands,
and the separate pinned Wanaku schema source. Check schema drift with:

```bash
cargo openapi --check crates/rhaf-api/openapi.json
```

## Run the embedded runtime

Core uses Wanaku's public `App::build` API and its Praxis/Pingora pipeline.
The executable pins Wanaku to commit
[`7af5411b9ede7b63fb1c3a664c3570d51ec67354`](https://github.com/wanaku-ai/wanaku/commit/7af5411b9ede7b63fb1c3a664c3570d51ec67354).
The dependency disables the upstream admin UI. The MCP and management handlers
share one registry, credential broker, and audit store.

Start Core with the default internal listeners:

```bash
cargo run --locked -- --data-dir ./data
```

The management listener uses `127.0.0.1:8080`. The MCP listener uses
`127.0.0.1:8081`. The default namespace endpoint is
`http://127.0.0.1:8081/default/mcp`. The runtime starts no inference or A2A
listener. Use `--management-listen` and `--mcp-listen` to change the addresses.
The upstream management API has no built-in caller authentication. Keep the
default loopback binding for local access. For remote access, place management
behind an authenticated proxy and restrict direct socket access to that proxy.
MCP governance and upstream credential bindings do not authorize management
callers.

Core creates `registry.json` and `audit-events.json` in the data directory.
Use a separate directory for each running instance. Core holds an exclusive
advisory lock on `.lock` until shutdown completes. Startup fails when another
Core instance holds the lock. Keep the directory to
restore registrations, tool enablement, and audit events after a restart.
Startup rejects unreadable or corrupt state, invalid bootstrap configuration,
and unavailable listener addresses.
Rejected bootstrap configuration does not publish registry or audit changes.

Core embeds this governance baseline:

```yaml
governance:
  default:
    mode: audit
    no_match: allow
    on_failure: allow
    audit_level: basic
audit:
  capture_payloads: false
  include_default_redaction_rules: true
  ingestion_enabled: false
```

The action-policy and evaluator filters stay in the pipeline. Valid unmatched
read and write calls pass both filters. Configured denials produce audit
observations and do not block admission in audit mode. The baseline has no
namespace overrides. Basic audit does not require an external LLM evaluator.
Protocol validation, credential checks, target availability, and backend
permissions still apply.

Native audit remains active and uses the persistent audit store. Payload
capture and external audit ingestion are disabled. Built-in redaction is
enabled. Storage failures produce diagnostics and do not block tool admission.
Audit does not establish private agent reasoning, verified user identity, or
lossless retention.

Pass `--wanaku-config PATH` to override the embedded Wanaku settings. Core
merges mapping fields and replaces scalar and list values. Omitted fields
keep their baseline values. The file must contain supported Wanaku keys.
Upstream `WANAKU_AUDIT_*` environment controls also apply.

Pass `--rhaf-config PATH` to load RHAF settings from a separate YAML file:

```yaml
search:
  max_results: 20
```

`search.max_results` must be a positive integer. The default is `20`. This
setting reserves the result limit for the progressive search implementation.
Core rejects unsupported RHAF keys. Do not put RHAF keys in the Wanaku file.

Start Core with both configuration files:

```bash
cargo run --locked -- --rhaf-config /path/to/rhaf.yaml --wanaku-config /path/to/wanaku.yaml --data-dir ./data
```

Credential bindings use Wanaku's environment resolver and the shared broker.
Use `env:VARIABLE_NAME` in a binding's `secretRefs` list. Set the variable in
the Core process environment. Keep secret values out of configuration files.
Core resolves brokered credentials only for HTTPS destinations. Credential-free
HTTP forwards remain available.
Keep credentials out of forward URLs, including query parameters. The pinned
Wanaku version can write these URLs to logs.

To combine concrete namespaces, add a virtual namespace to the bootstrap file:

```yaml
namespaces:
  - name: backend
  - name: portal
    members: [backend]
```

Register forwards in `backend`. Use
`http://127.0.0.1:8081/virtual-ns/portal/mcp` to access the combined inventory.
Tool names include the concrete namespace, for example `backend/read_state`.
Virtual namespace members must be concrete namespaces. Registry persistence
retains namespace definitions and memberships after a restart.

Send `SIGTERM` to stop Core. Pingora drains requests, then Core flushes registry
and audit persistence before exit. The default drain timeout is 10 seconds.
Use `--shutdown-timeout-secs` to change it. `SIGINT` requests Pingora's fast
shutdown path.

The client-facing MCP endpoint is stateless. Initialization returns no
`Mcp-Session-Id`. Each client can initialize, send `notifications/initialized`,
list tools, and call tools independently. Initialize clients again after a
restart. The integration check verifies this behavior against the pinned
runtime. This runtime does not add a WASM tool host, full lifecycle concurrency,
or healthy-forward catalog refresh.

## Verify real MCP forwarding

Use Java 21 or later and Maven 3.9 or later for the controlled HTTP MCP fixture.
Normal Rust builds and tests do not require Java or Maven. Build Core and the
fixture:

```bash
cargo build --workspace --locked
git clone --branch main https://github.com/rhaf-ai/rhaf-portal-tests.git /tmp/rhaf-portal-tests
mvn -B -f /tmp/rhaf-portal-tests/pom.xml -pl fixtures/test-mcp-server -am package -DskipTests
RHAF_MCP_FIXTURE_JAR=/tmp/rhaf-portal-tests/fixtures/test-mcp-server/target/quarkus-app/quarkus-run.jar \
RHAF_MCP_FIXTURE_REVISION="$(git -C /tmp/rhaf-portal-tests rev-parse HEAD)" \
cargo test --locked --test runtime_integration -- --ignored --nocapture
```

The Rust integration check starts separate fixture and Core processes on local
ports. Regular `cargo test` runs the harness
regression tests without the external fixture. The explicit integration command
fails if the fixture artifact or revision is missing.

The check registers the fixture through management, initializes two MCP clients, and
executes typed write and read calls through the real MCP listener. It compares
the results with the fixture's independent call observations. It also checks
shared tool enablement, disabled UI routes, startup failures, graceful shutdown,
exclusive data-directory ownership, and registry and audit recovery after restart.
It checks virtual namespace catalogs, qualified calls, and membership recovery.
It removes the test forward and stops both processes.

Each run retains logs, fixture observations, configuration, and `result.json`
under `target/runtime-evidence/run-*`. The dedicated `Integration Tests` reusable
workflow runs after the PR build, lint, and container jobs succeed. It checks
out `main` from `rhaf-portal-tests` and runs the Java integration suite against
the Core binary built from the PR's merge revision. The job summary records the
tests commit. The Java build also
packages the fixture for the Rust integration check. The workflow uploads
`portal-integration-evidence` and `runtime-integration-evidence`.

## Container image

The multi-stage `Containerfile` uses Red Hat UBI 10.1 to build the binary and
UBI 10.1 Minimal to run it as a non-root user. The image includes the OpenSSL
runtime libraries and starts the embedded runtime.

Build with Docker or Podman:

```bash
cargo build-image --tag localhost/rhaf-portal-core:dev
```

Run the image with Docker when Docker is installed:

```bash
docker run --rm localhost/rhaf-portal-core:dev-aarch64 --version
```

If Docker is not installed, run the image with Podman:

```bash
podman run --rm localhost/rhaf-portal-core:dev-aarch64 --version
```

Mount a writable data directory to retain state. Set the listener addresses to
`0.0.0.0:8080` and `0.0.0.0:8081` inside the container when you publish these
ports. Publish management only through the authenticated proxy boundary
described above.

Use `-x86_64` on an Intel or AMD host. The build command adds the native
architecture suffix. Docker takes precedence when `docker version` succeeds;
otherwise the build uses Podman. A stopped Docker daemon or missing socket
permission therefore selects Podman. Use the tool shown in the build output to
run the image.

To publish a multi-architecture image, run the first command on both Linux x86_64
and ARM64 hosts. Then run the second command once. Manifest publication requires
Podman and registry authentication.

```bash
cargo build-image --push --tag REGISTRY/GROUP/rhaf-portal-core:TAG
cargo build-image-manifest --tag REGISTRY/GROUP/rhaf-portal-core:TAG
```

## CI and releases

- `pr-build`: build, unit tests, formatting, Clippy, and container builds.
- `integration-tests`: run the Java Portal suite and Rust MCP check after the PR build, lint, and container jobs succeed.
- `main-build`: Linux x86_64 and ARM64 builds and container publication.
- `lychee-links`: Markdown links and formatting.
- `early-access`: manually publish four-platform archives and rolling prereleases.
- `release`: publish versioned archives and images from a version tag or manual run.

Container publication uses organization variables `CONTAINER_REGISTRY` and
`CONTAINER_GROUP`, plus secrets `QUAY_USERNAME` and `QUAY_PASSWORD`.
JReleaser signs release archives with secrets `GPG_PUBLIC_KEY`, `GPG_PRIVATE_KEY`,
and `MAVEN_GPG_PASSPHRASE`, following Wanaku's release setup. GitHub releases use
the workflow's `GITHUB_TOKEN`.

For early access, supply the Cargo package version (`0.1.0-dev` initially).
For a stable release, first update the workspace version and `Cargo.lock` to the
release version. Push a matching version tag, such as `v0.1.0`, or supply the
version to the manual release workflow. Version validation runs before publication.

Dependabot checks Cargo dependencies and GitHub Actions weekly.

## License

[Apache License 2.0](LICENSE).
