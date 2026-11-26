use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::method};

use super::{ExitPolicy, Harness};

const MARKER: &str = "forward-url-redaction-canary";
const PATHS: [&str; 3] = ["/successful", "/failed-listing", "/failed-handshake"];

impl Harness {
    pub async fn verify_forward_url_redaction(&self) -> Result<()> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(discovery_response)
            .mount(&server)
            .await;
        let query = format!("access_token={MARKER}&encoded=a%2Fb&encoded=c");
        self.write_discovery_config(&server.uri(), &query)?;

        let mut command = self.core_command(&self.config(), &self.data_dir());
        command.env("RUST_LOG", "debug");
        let log_name = "forward-url-redaction.log";
        self.start(
            command,
            log_name,
            &self.management_url("/api/v1/tools"),
            ExitPolicy::Core,
        )
        .await?
        .stop()
        .await?;

        let requests = server
            .received_requests()
            .await
            .context("record MCP requests")?;
        verify_request_targets(&requests, &query)?;
        verify_discovery_log(&std::fs::read_to_string(self.evidence.join(log_name))?)
    }

    fn write_discovery_config(&self, origin: &str, query: &str) -> Result<()> {
        let forwards = PATHS.map(|path| {
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

fn verify_request_targets(requests: &[Request], query: &str) -> Result<()> {
    for path in PATHS {
        ensure!(
            requests.iter().any(|request| request.url.path() == path),
            "no MCP request reached {path}"
        );
    }
    ensure!(
        requests.iter().all(|request| {
            PATHS.contains(&request.url.path()) && request.url.query() == Some(query)
        }),
        "Core changed the forward request target"
    );
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
        Some("initialize") => json!({
            "protocolVersion": "2025-11-25",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "controlled-fixture", "version": "1" },
        }),
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

fn failed_listing(id: &Value, request: &Request) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32603, "message": format!("listing failed at {}", request.url) },
    }))
}
