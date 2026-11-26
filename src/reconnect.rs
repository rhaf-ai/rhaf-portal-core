use std::sync::Arc;
use std::time::Duration;

use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;
use wanaku_infra::credentials::{CredentialAuditSink, CredentialBroker};
use wanaku_server::management::{apply_forward_update, prepare_forward_update};
use wanaku_types::registry::ForwardRegistry;

use rhaf_portal_core::lifecycle::{Lifecycle, validate_refresh};

pub struct Reconnect {
    pub lifecycle: Lifecycle,
    pub broker: Arc<CredentialBroker>,
    pub audit_sink: Arc<dyn CredentialAuditSink>,
    pub interval: Duration,
}

impl Reconnect {
    async fn refresh_unavailable(&self) {
        let forwards = self.lifecycle.commit(ForwardRegistry::list_forwards).await;
        for forward in forwards.into_iter().filter(|forward| !forward.available) {
            let request = self
                .lifecycle
                .commit(|registry| {
                    let request = registry.begin_forward_refresh(&forward.name)?;
                    (!request.target().available
                        && validate_refresh(registry, request.target()).is_ok())
                    .then_some(request)
                })
                .await;
            let Some(request) = request else {
                continue;
            };
            let target = request.target().clone();
            let prepared =
                prepare_forward_update(&self.broker, Some(self.audit_sink.as_ref()), request).await;
            let _commit = self.lifecycle.commit_guard().await;
            let registry = self.lifecycle.registry();
            if validate_refresh(registry, &target).is_ok() {
                // Wanaku checks the probe generation before publishing either outcome.
                let _ = apply_forward_update(registry, prepared);
            }
        }
    }
}

#[async_trait::async_trait]
impl BackgroundService for Reconnect {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        let mut ticker = tokio::time::interval(self.interval);
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => Box::pin(self.refresh_unavailable()).await,
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    }
}
