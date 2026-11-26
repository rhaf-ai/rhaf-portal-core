//! Real MCP forwarding and process lifecycle evidence.

mod support;

use anyhow::Result;
use serde_json::json;
use support::Harness;

#[tokio::test]
async fn forward_discovery_redacts_credential_bearing_urls() -> Result<()> {
    Harness::management_only()?
        .verify_forward_url_redaction()
        .await
}

#[tokio::test]
async fn management_wrapper_without_external_services() -> Result<()> {
    let harness = Harness::management_only()?;
    let core = harness.start_core("management.log").await?;
    harness.verify_management_wrapper().await?;
    harness.verify_unavailable_registration().await?;
    core.stop().await
}

#[tokio::test]
#[ignore = "requires RHAF_MCP_FIXTURE_JAR and RHAF_MCP_FIXTURE_REVISION"]
async fn conventional_runtime_against_controlled_mcp_fixture() -> Result<()> {
    let harness = Harness::from_env()?;
    harness.verify_startup().await?;
    harness.verify_opa_redaction().await?;
    harness.verify_audit_shutdown_failure().await?;
    let fixture = harness.start_fixture().await?;
    harness.verify_rejected_bootstrap().await?;
    let first = harness.start_core("core-first.log").await?;
    harness.verify_exclusive_storage().await?;
    let expected = harness.exercise().await?;
    first.stop().await?;
    anyhow::ensure!(
        harness.data_dir().join("registry.json").is_file(),
        "registry shutdown flush missing"
    );
    let second = harness.start_core("core-restart.log").await?;
    harness.verify_restart(&expected).await?;
    harness.verify_lifecycle_replacement().await?;
    second.stop().await?;
    harness
        .verify_observational_deny_and_storage_failure(&expected)
        .await?;
    fixture.stop().await?;
    harness.record_success()
}

#[test]
fn nested_json_boolean_is_distinct_from_number() {
    let expected = json!({"metadata": {"samples": [1, false]}});
    assert_eq!(expected, json!({"metadata": {"samples": [1, false]}}));
    assert_ne!(expected, json!({"metadata": {"samples": [1, 0]}}));
}
