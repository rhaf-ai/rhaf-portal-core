use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use reqwest::Method;
use serde_json::{Value, json};

use super::{Expected, Harness, http::names};

impl Harness {
    pub async fn exercise(&self) -> Result<Expected> {
        self.register_forward().await?;
        self.register_virtual_namespace().await?;
        self.http
            .initialize(&self.mcp_url(), "first-client")
            .await?;
        self.http
            .initialize(&self.virtual_mcp_url(), "second-client")
            .await?;
        self.verify_shared_registry().await?;
        let state = self.verify_real_calls().await?;
        self.set_read_enabled(false).await?;
        self.verify_refresh_preserves_enablement().await?;
        self.verify_virtual_catalog().await?;
        Ok(Expected {
            state,
            audit_ids: self.capture_audit_ids().await?,
        })
    }

    async fn verify_refresh_preserves_enablement(&self) -> Result<()> {
        let tools = self.http.tools(&self.mcp_url()).await?;
        ensure!(
            tools == BTreeSet::from(["set_state".to_owned()]),
            "management update was not shared"
        );
        let refresh = self
            .http
            .request(
                Method::POST,
                &self.management_url("/api/v1/forwards/controlled-fixture/refreshes"),
                None,
            )
            .await?;
        ensure!(
            refresh.status == 200 && refresh.body["data"]["tools_discovered"] == 2,
            "forward refresh failed: {}",
            refresh.body
        );
        ensure!(
            self.http.tools(&self.mcp_url()).await? == tools,
            "forward refresh changed tool enablement"
        );
        Ok(())
    }

    pub(super) async fn register_forward(&self) -> Result<()> {
        let reply = self
            .post_forward("controlled-fixture", &self.fixture_url("/mcp"))
            .await?;
        ensure!(
            reply.status == 200,
            "forward creation failed: {}",
            reply.body
        );
        ensure!(
            reply.body["data"]["tools_discovered"] == 2,
            "fixture discovery failed: {}",
            reply.body
        );
        Ok(())
    }

    async fn register_virtual_namespace(&self) -> Result<()> {
        let reply = self
            .http
            .request(
                Method::POST,
                &self.management_url("/api/v1/namespaces"),
                Some(&json!({"name": "integration", "members": ["default"]})),
            )
            .await?;
        ensure!(
            reply.status == 200,
            "virtual namespace creation failed: {}",
            reply.body
        );
        Ok(())
    }

    async fn verify_shared_registry(&self) -> Result<()> {
        let tools = self.http.tools(&self.mcp_url()).await?;
        ensure!(
            tools == BTreeSet::from(["read_state".to_owned(), "set_state".to_owned()]),
            "MCP catalog differs from fixture"
        );
        let managed = self
            .http
            .request(Method::GET, &self.management_url("/api/v1/tools"), None)
            .await?;
        ensure!(managed.status == 200, "management tool listing failed");
        ensure!(
            names(&managed.body["data"])? == tools,
            "management and MCP registries differ"
        );
        self.verify_virtual_catalog().await?;
        Ok(())
    }

    async fn verify_virtual_catalog(&self) -> Result<()> {
        let expected = self
            .http
            .tools(&self.mcp_url())
            .await?
            .into_iter()
            .map(|name| format!("default/{name}"))
            .collect::<BTreeSet<_>>();
        ensure!(
            self.http.tools(&self.virtual_mcp_url()).await? == expected,
            "virtual catalog does not reflect its concrete member"
        );
        Ok(())
    }

    async fn verify_real_calls(&self) -> Result<Value> {
        let arguments = json!({"target": "runtime-issue-5", "value": 37, "enabled": true,
            "metadata": {"source": "integration", "samples": [1, false]}});
        let written = self
            .http
            .call(&self.mcp_url(), "set_state", arguments.clone())
            .await?;
        let read = self
            .http
            .call(
                &self.virtual_mcp_url(),
                "default/read_state",
                json!({"target": arguments["target"]}),
            )
            .await?;
        let expected = json!({"target": arguments["target"], "state": {
            "value": arguments["value"], "enabled": arguments["enabled"], "metadata": arguments["metadata"],
        }});
        ensure!(
            written == expected && read == expected,
            "wrong real tool result: {written}, {read}"
        );
        self.verify_observations(&arguments).await?;
        Ok(expected)
    }

    async fn verify_observations(&self, arguments: &Value) -> Result<()> {
        let reply = self
            .http
            .request(Method::GET, &self.fixture_url("/control/state"), None)
            .await?;
        ensure!(
            reply.status == 200 && reply.body["callCount"] == 2,
            "unexpected fixture attempts: {}",
            reply.body
        );
        let observations = reply.body["observations"]
            .as_array()
            .context("fixture observations missing")?;
        verify_observed_arguments(observations, arguments)?;
        self.save_json("fixture-observations.json", &reply.body)
    }

    async fn set_read_enabled(&self, enabled: bool) -> Result<()> {
        let action = if enabled { "enable" } else { "disable" };
        let reply = self
            .http
            .request(
                Method::PUT,
                &self.management_url(&format!("/api/v1/tools/read_state/{action}")),
                None,
            )
            .await?;
        ensure!(reply.status == 200, "management tool {action} failed");
        Ok(())
    }

    pub(super) async fn audit_events(&self) -> Result<Vec<Value>> {
        let reply = self
            .http
            .request(
                Method::GET,
                &self.management_url("/api/v1/audit/events"),
                None,
            )
            .await?;
        ensure!(reply.status == 200, "cannot query audit events");
        reply.body["data"]["events"]
            .as_array()
            .cloned()
            .context("audit event array missing")
    }

    async fn capture_audit_ids(&self) -> Result<BTreeSet<String>> {
        let events = self.audit_events().await?;
        let relevant: Vec<_> = events
            .iter()
            .filter(|event| {
                event["operation"] == "tools/call"
                    && matches!(event["target"].as_str(), Some("read_state" | "set_state"))
            })
            .collect();
        let targets: BTreeSet<_> = relevant
            .iter()
            .filter_map(|event| event["target"].as_str())
            .collect();
        ensure!(
            targets == BTreeSet::from(["read_state", "set_state"]),
            "tool-call audit events missing before shutdown"
        );
        verify_baseline_observations(&relevant)?;
        let ids = event_ids(relevant.into_iter())?;
        self.save_json("expected-audit-ids.json", &ids)?;
        Ok(ids)
    }

    pub async fn verify_restart(&self, expected: &Expected) -> Result<()> {
        self.http
            .initialize(&self.virtual_mcp_url(), "after-restart")
            .await?;
        self.verify_recovered_registry().await?;
        self.verify_recovered_namespace().await?;
        self.verify_recovered_audit(&expected.audit_ids).await?;
        self.verify_recovered_call(&expected.state).await?;
        self.verify_no_replay_and_cleanup().await
    }

    async fn verify_recovered_registry(&self) -> Result<()> {
        ensure!(
            self.http.tools(&self.mcp_url()).await? == BTreeSet::from(["set_state".to_owned()]),
            "persisted disabled tool state was lost"
        );
        let forward = self
            .http
            .request(
                Method::GET,
                &self.management_url("/api/v1/forwards/controlled-fixture"),
                None,
            )
            .await?;
        ensure!(
            forward.status == 200 && forward.body["data"]["address"] == self.fixture_url("/mcp"),
            "persisted forward was lost"
        );
        Ok(())
    }

    async fn verify_recovered_namespace(&self) -> Result<()> {
        let namespace = self
            .http
            .request(
                Method::GET,
                &self.management_url("/api/v1/namespaces/integration"),
                None,
            )
            .await?;
        ensure!(
            namespace.status == 200 && namespace.body["data"]["members"] == json!(["default"]),
            "persisted virtual namespace was lost"
        );
        self.verify_virtual_catalog().await?;
        Ok(())
    }

    async fn verify_recovered_audit(&self, audit_ids: &BTreeSet<String>) -> Result<()> {
        let recovered = event_ids(self.audit_events().await?.iter())?;
        ensure!(
            audit_ids.is_subset(&recovered),
            "first-run tool-call audit events were lost"
        );
        ensure!(
            self.data_dir().join("audit-events.json").is_file(),
            "audit persistence file missing"
        );
        Ok(())
    }

    async fn verify_recovered_call(&self, expected: &Value) -> Result<()> {
        self.set_read_enabled(true).await?;
        ensure!(
            self.http
                .tools(&self.mcp_url())
                .await?
                .contains("read_state"),
            "recovered registry is not shared"
        );
        self.verify_virtual_catalog().await?;
        let result = self
            .http
            .call(
                &self.virtual_mcp_url(),
                "default/read_state",
                json!({"target": expected["target"]}),
            )
            .await?;
        ensure!(
            result == *expected,
            "recovered forward did not return real state"
        );
        Ok(())
    }

    async fn verify_no_replay_and_cleanup(&self) -> Result<()> {
        let observed = self
            .http
            .request(Method::GET, &self.fixture_url("/control/state"), None)
            .await?;
        ensure!(observed.body["callCount"] == 3, "restart replayed a call");
        let deleted = self
            .http
            .request(
                Method::DELETE,
                &self.management_url("/api/v1/forwards/controlled-fixture"),
                None,
            )
            .await?;
        ensure!(
            deleted.status == 200 && self.http.tools(&self.mcp_url()).await?.is_empty(),
            "forward cleanup did not update MCP"
        );
        self.verify_virtual_catalog().await?;
        Ok(())
    }
}

fn verify_baseline_observations(events: &[&Value]) -> Result<()> {
    for target in ["read_state", "set_state"] {
        for filter in ["wanaku_action_policy", "wanaku_evaluator"] {
            ensure!(
                events
                    .iter()
                    .any(|event| event["target"] == target && event["filter"] == filter),
                "missing {filter} observation for unmatched {target}"
            );
        }
    }
    ensure!(
        events.iter().all(|event| {
            event.get("payload").is_none() && event["redaction"]["payload_captured"] == false
        }),
        "default audit captured tool payloads"
    );
    Ok(())
}

fn verify_observed_arguments(observations: &[Value], arguments: &Value) -> Result<()> {
    let observed = observations.first().context("no fixture invocation")?;
    ensure!(
        observed["arguments"] == *arguments,
        "argument types changed"
    );
    let read = observations
        .get(1)
        .context("read_state invocation missing")?;
    ensure!(
        read["arguments"] == json!({"target": arguments["target"]}),
        "read_state arguments changed"
    );
    ensure!(
        observations.iter().all(|row| row["outcome"] == "success"),
        "fixture observed a failed invocation"
    );
    Ok(())
}

fn event_ids<'a>(events: impl Iterator<Item = &'a Value>) -> Result<BTreeSet<String>> {
    events
        .map(|event| {
            event["event_id"]
                .as_str()
                .map(str::to_owned)
                .context("audit event ID missing")
        })
        .collect()
}

#[cfg(test)]
mod observation_tests {
    use anyhow::Result;
    use serde_json::json;

    #[test]
    fn rejects_extra_arguments_in_read_observation() -> Result<()> {
        let arguments = json!({"target": "test", "value": 37, "enabled": true});
        let mut observations = vec![
            json!({"arguments": arguments, "outcome": "success"}),
            json!({"arguments": {"target": "test"}, "outcome": "success"}),
        ];
        super::verify_observed_arguments(&observations, &arguments)?;
        observations[1]["arguments"]["extra"] = json!(true);
        assert!(super::verify_observed_arguments(&observations, &arguments).is_err());
        Ok(())
    }
}
