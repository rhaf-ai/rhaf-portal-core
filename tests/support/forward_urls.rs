use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

use super::{ExitPolicy, Harness};

const MARKER: &str = "forward-url-redaction-canary";
const BOOTSTRAP_PATHS: [&str; 3] = ["/successful", "/failed-listing", "/failed-handshake"];
const SESSION_PATH: &str = "/sessionful";
const SESSION_ID: &str = "controlled-session";

impl Harness {
    pub async fn verify_forward_url_redaction(&self) -> Result<()> {
        let server = discovery_fixture().await;
        let query = format!("access_token={MARKER}&encoded=a%2Fb&encoded=c");
        self.write_discovery_config(&server.uri(), &query)?;

        let mut command = self.core_command(&self.config(), &self.data_dir());
        command.env("RUST_LOG", "debug");
        let log_name = "forward-url-redaction.log";
        let core = self
            .start(
                command,
                log_name,
                &self.management_url("/api/v1/tools"),
                ExitPolicy::Core,
            )
            .await?;
        self.register_sessionful_forward(&server.uri(), &query)
            .await?;
        let log = self.evidence.join(log_name);
        wait_for_session_diagnostics(&log).await?;
        core.stop().await?;

        let requests = server
            .received_requests()
            .await
            .context("record MCP requests")?;
        verify_request_targets(&requests, &query)?;
        verify_discovery_log(&std::fs::read_to_string(log)?)
    }

    async fn register_sessionful_forward(&self, origin: &str, query: &str) -> Result<()> {
        let reply = self
            .post_forward("sessionful", &format!("{origin}{SESSION_PATH}?{query}"))
            .await?;
        ensure!(
            reply.status == 200,
            "management registration failed ({})",
            reply.status
        );
        Ok(())
    }

    fn write_discovery_config(&self, origin: &str, query: &str) -> Result<()> {
        let forwards = BOOTSTRAP_PATHS.map(|path| {
            json!({
                "name": path.trim_start_matches('/'),
                "address": format!("{origin}{path}?{query}"),
            })
        });
        std::fs::write(
            self.config(),
            serde_json::to_string(&json!({ "forwards": forwards }))?,
        )?;
        Ok(())
    }
}

async fn discovery_fixture() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(discovery_response)
        .mount(&server)
        .await;
    for (verb, status) in [("GET", 418), ("DELETE", 500)] {
        Mock::given(method(verb))
            .and(path(SESSION_PATH))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
    }
    server
}

async fn wait_for_session_diagnostics(log: &Path) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let output = std::fs::read_to_string(log)?;
            if output.contains("fail to get common stream")
                && output.contains("fail to delete session")
            {
                return Ok::<_, std::io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("session GET and DELETE failures were not logged")??;
    Ok(())
}

fn verify_request_targets(requests: &[Request], query: &str) -> Result<()> {
    for path in BOOTSTRAP_PATHS.into_iter().chain([SESSION_PATH]) {
        ensure!(
            requests.iter().any(|request| request.url.path() == path),
            "no MCP request reached {path}"
        );
    }
    ensure!(
        requests.iter().all(|request| {
            (BOOTSTRAP_PATHS.contains(&request.url.path()) || request.url.path() == SESSION_PATH)
                && request.url.query() == Some(query)
        }),
        "Core changed the forward request target"
    );
    for verb in ["GET", "DELETE"] {
        ensure!(
            requests.iter().any(|request| {
                request.method.as_str() == verb
                    && request.url.path() == SESSION_PATH
                    && request
                        .headers
                        .get("mcp-session-id")
                        .is_some_and(|id| id == SESSION_ID)
            }),
            "session {verb} did not reach the fixture with the session header"
        );
    }
    Ok(())
}

fn verify_discovery_log(log: &str) -> Result<()> {
    ensure!(
        !log.contains(MARKER),
        "forward query marker appeared in Core output"
    );
    for diagnostic in [
        "registered forward from config",
        "discovering from forward",
        "captured MCP server identity",
        "connected to MCP server for forward discovery",
        "listing complete",
        "listing failed during discovery",
        "MCP handshake failed",
        "registered forward via management API",
    ] {
        ensure!(log.contains(diagnostic), "missing diagnostic: {diagnostic}");
    }
    Ok(())
}

fn discovery_response(request: &Request) -> ResponseTemplate {
    if request.url.path() == "/failed-handshake" {
        return ResponseTemplate::new(500);
    }
    let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
        return ResponseTemplate::new(400);
    };
    let result = match message["method"].as_str() {
        Some("initialize") => return initialize_response(&message["id"], request.url.path()),
        Some("notifications/initialized") => return ResponseTemplate::new(202),
        Some("tools/list") if request.url.path() == "/failed-listing" => {
            return failed_listing(&message["id"], request);
        }
        Some("tools/list") => json!({ "tools": [] }),
        Some("resources/list") => json!({ "resources": [] }),
        Some("resources/templates/list") => json!({ "resourceTemplates": [] }),
        Some("prompts/list") => json!({ "prompts": [] }),
        _ => return ResponseTemplate::new(400),
    };
    ResponseTemplate::new(200).set_body_json(json!({
        "jsonrpc": "2.0",
        "id": message["id"],
        "result": result,
    }))
}

fn initialize_response(id: &Value, path: &str) -> ResponseTemplate {
    let response = ResponseTemplate::new(200).set_body_json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2025-11-25",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "controlled-fixture", "version": "1" },
        },
    }));
    if path == SESSION_PATH {
        response.insert_header("mcp-session-id", SESSION_ID)
    } else {
        response
    }
}

fn failed_listing(id: &Value, request: &Request) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32603, "message": format!("listing failed at {}", request.url) },
    }))
}
