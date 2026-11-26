mod governance;
pub mod http;
mod lifecycle;
mod management;
mod process;
mod scenario;
mod startup;

use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};

use http::Http;
use process::{ExitPolicy, Running};

const CORE: &str = env!("CARGO_BIN_EXE_rhaf-portal-core");

pub struct Harness {
    http: Http,
    evidence: PathBuf,
    fixture_jar: PathBuf,
    fixture_revision: String,
    fixture: SocketAddr,
    management: SocketAddr,
    mcp: SocketAddr,
}

pub struct Expected {
    state: Value,
    audit_ids: BTreeSet<String>,
}

impl Harness {
    pub fn from_env() -> Result<Self> {
        let fixture_jar = PathBuf::from(
            std::env::var_os("RHAF_MCP_FIXTURE_JAR")
                .context("set RHAF_MCP_FIXTURE_JAR to the controlled fixture JAR")?,
        );
        let fixture_revision = fixture_revision()?;
        ensure!(
            fixture_jar.is_file(),
            "fixture JAR missing: {}",
            fixture_jar.display()
        );
        Self::with_fixture(fixture_jar, fixture_revision)
    }

    pub fn management_only() -> Result<Self> {
        Self::with_fixture(PathBuf::new(), String::new())
    }

    fn with_fixture(fixture_jar: PathBuf, fixture_revision: String) -> Result<Self> {
        let evidence = evidence_directory()?;
        let [fixture, management, mcp] = reserve_addresses()?;
        println!("Evidence: {}", evidence.display());
        let harness = Self {
            http: Http::new()?,
            evidence,
            fixture_jar,
            fixture_revision,
            fixture,
            management,
            mcp,
        };
        std::fs::write(harness.config(), "{}\n")?;
        Ok(harness)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.evidence.join("data")
    }
    fn config(&self) -> PathBuf {
        self.evidence.join("wanaku.yaml")
    }
    fn management_url(&self, path: &str) -> String {
        format!("http://{}{path}", self.management)
    }
    fn fixture_url(&self, path: &str) -> String {
        format!("http://{}{path}", self.fixture)
    }
    fn mcp_url(&self) -> String {
        format!("http://{}/default/mcp", self.mcp)
    }

    fn virtual_mcp_url(&self) -> String {
        format!("http://{}/virtual-ns/integration/mcp", self.mcp)
    }

    fn core_command(&self, config: &Path, directory: &Path) -> Command {
        let mut command = Command::new(CORE);
        command
            .args([
                "--mcp-listen",
                &self.mcp.to_string(),
                "--management-listen",
                &self.management.to_string(),
            ])
            .arg("--data-dir")
            .arg(directory)
            .arg("--wanaku-config")
            .arg(config)
            .args(["--shutdown-timeout-secs", "1"]);
        command
    }

    async fn start(
        &self,
        command: Command,
        log: &str,
        ready: &str,
        policy: ExitPolicy,
    ) -> Result<Running> {
        let mut process = Running::spawn(command, &self.evidence.join(log), policy)?;
        process.wait_ready(&self.http, ready).await?;
        Ok(process)
    }

    pub async fn start_core(&self, log: &str) -> Result<Running> {
        self.start(
            self.core_command(&self.config(), &self.data_dir()),
            log,
            &self.management_url("/api/v1/tools"),
            ExitPolicy::Core,
        )
        .await
    }

    pub async fn start_fixture(&self) -> Result<Running> {
        let mut command = Command::new("java");
        command
            .arg(format!("-Dquarkus.http.port={}", self.fixture.port()))
            .args(["-Dquarkus.http.host=127.0.0.1", "-jar"])
            .arg(&self.fixture_jar);
        self.start(
            command,
            "fixture.log",
            &self.fixture_url("/health"),
            ExitPolicy::Fixture,
        )
        .await
    }

    fn save_json(&self, name: &str, value: &impl Serialize) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        std::fs::write(self.evidence.join(name), bytes)?;
        Ok(())
    }

    pub fn record_success(&self) -> Result<()> {
        let output = Command::new(CORE).arg("--version").output()?;
        ensure!(output.status.success(), "cannot record Core version");
        let version = String::from_utf8(output.stdout)?;
        let result = json!({
            "result": "passed", "core_version": version.trim(), "fixture_revision": self.fixture_revision,
            "checks": [
                "persistent audit write failure produces a nonzero shutdown exit",
                "shipped governance admits unmatched reads and writes through both filters",
                "configured deny remains observational and storage degradation permits real calls",
                "rejected bootstrap preserves existing registry and audit bytes",
                "malformed OPA diagnostics redact values and preserve deny state",
                "concurrent data-directory use rejected and lock released on shutdown",
                "typed real tool results and exact fixture observations",
                "invalid configuration and occupied listener rejected before startup",
                "two stateless client handshakes and ping",
                "shared management and MCP registry before and after restart",
                "manual forward refresh preserves disabled tool state",
                "virtual namespace qualified catalog, dispatch, updates, and restart recovery",
                "registry state and exact tool-call audit IDs survive SIGTERM shutdown",
                "forward recovery without replay",
                "lifecycle replacement preserves identity and commits before the response",
                "failed lifecycle discovery preserves the prior registration and capabilities",
            ],
        });
        self.save_json("result.json", &result)?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        Ok(())
    }
}

fn reserve_addresses() -> Result<[SocketAddr; 3]> {
    let fixture = TcpListener::bind("127.0.0.1:0")?;
    let management = TcpListener::bind("127.0.0.1:0")?;
    let mcp = TcpListener::bind("127.0.0.1:0")?;
    Ok([
        fixture.local_addr()?,
        management.local_addr()?,
        mcp.local_addr()?,
    ])
}

fn evidence_directory() -> Result<PathBuf> {
    let base = std::env::var_os("RHAF_RUNTIME_EVIDENCE_DIR")
        .map_or_else(|| PathBuf::from("target/runtime-evidence"), PathBuf::from);
    std::fs::create_dir_all(&base)?;
    Ok(tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(base)?
        .keep()
        .canonicalize()?)
}

fn fixture_revision() -> Result<String> {
    let revision = std::env::var("RHAF_MCP_FIXTURE_REVISION")
        .context("set RHAF_MCP_FIXTURE_REVISION to the fixture commit")?;
    ensure!(!revision.trim().is_empty(), "fixture revision is empty");
    Ok(revision)
}
