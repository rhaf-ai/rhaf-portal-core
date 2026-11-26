//! Embedded Wanaku runtime for RHAF Portal Core.

mod config;
mod credentials;
mod feature_config;
mod listeners;
mod management;
mod persistence;
mod reconnect;
mod runtime;
mod startup;

use clap::Parser;

const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (Wanaku 1a22a9e692e2f50aed56913000be3def38a7ca2c)"
);

const ASSEMBLY_ERROR: &str =
    "Wanaku application assembly failed; check bootstrap feature and pipeline configuration";

fn main() -> anyhow::Result<()> {
    let args = startup::Args::parse();
    let rhaf = config::RhafConfig::load(args.rhaf_config.as_deref())?;
    let pipeline = args.pipeline()?;
    let _tracing = praxis_core::logging::init_tracing(&pipeline)?;
    tracing::debug!(
        search_max_results = rhaf.search.max_results.get(),
        "RHAF configuration loaded"
    );
    praxis_tls::provider::install();
    anyhow::ensure!(
        praxis_tls::provider::installed(),
        "TLS provider unavailable"
    );
    run(&args, &pipeline)
}

fn run(args: &startup::Args, pipeline: &praxis_core::config::Config) -> anyhow::Result<()> {
    let bootstrap = startup::load_bootstrap(args.wanaku_config.as_deref())?;
    let governance = startup::governance(bootstrap.as_ref())?;
    let _data_lock = startup::lock_data_directory(&args.data_dir)?;
    let management = startup::bind_management(args.management_listen)?;
    let (resources, persistence) = startup::resources(&args.data_dir)?;
    startup::validate_lifecycle_bootstrap(bootstrap.as_ref(), persistence.as_ref())?;
    let app = wanaku_server::app::App::build(
        &wanaku_server::app::AppSettings {
            pipeline,
            wanaku_config: bootstrap.as_ref(),
            governance,
            plugins_dir: None,
        },
        resources,
    )
    .map_err(|_| anyhow::anyhow!(ASSEMBLY_ERROR))?;
    persistence.activate()?;
    let runtime_result = runtime::run(args, pipeline, app, management);
    let audit_result = persistence.retry_failed_audit(std::time::Duration::from_secs(10));
    runtime_result.and(audit_result)?;
    tracing::info!("RHAF Portal Core stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result};

    const REQUIRED_DEPENDENCIES: [&str; 4] = [
        "wanaku-server",
        "wanaku-infra",
        "wanaku-types",
        "wanaku-feature-audit",
    ];

    #[test]
    fn version_matches_every_wanaku_dependency_revision() -> Result<()> {
        verify_revisions(include_str!("../Cargo.toml"))
    }

    fn verify_revisions(manifest: &str) -> Result<()> {
        let manifest: toml::Value = toml::from_str(manifest)?;
        let dependencies = manifest
            .get("dependencies")
            .and_then(toml::Value::as_table)
            .context("dependencies missing")?;
        for (name, declaration) in dependencies
            .iter()
            .filter(|(name, _)| name.starts_with("wanaku-"))
        {
            let revision = declaration
                .get("rev")
                .and_then(toml::Value::as_str)
                .with_context(|| format!("{name} must pin a revision"))?;
            anyhow::ensure!(
                revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "{name} must pin a full commit SHA"
            );
            anyhow::ensure!(
                super::VERSION.contains(revision),
                "VERSION differs from {name}"
            );
        }
        for name in REQUIRED_DEPENDENCIES {
            assert!(dependencies.contains_key(name));
        }
        Ok(())
    }

    #[test]
    fn revision_comment_does_not_replace_a_dependency_pin() -> Result<()> {
        let manifest = include_str!("../Cargo.toml");
        let (before, after) = manifest
            .split_once("wanaku-server = ")
            .context("server dependency missing")?;
        let (_, remainder) = after
            .split_once('\n')
            .context("server dependency line missing")?;
        let unpinned = format!(
            "{before}wanaku-server = {{ git = 'https://github.com/wanaku-ai/wanaku', branch = 'main' }} # rev = '1a22a9e692e2f50aed56913000be3def38a7ca2c'\n{remainder}"
        );
        assert!(verify_revisions(&unpinned).is_err());
        Ok(())
    }
}
