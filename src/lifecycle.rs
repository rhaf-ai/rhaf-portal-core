mod write;

pub use write::{WriteError, WriteMode, WriteOutcome};

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use rhaf_api::models::{CredentialBindings, Forward, LifecycleState};
use tokio::sync::{Mutex, MutexGuard};
use uuid::Uuid;
use wanaku_infra::registry::InMemoryRegistry;
use wanaku_types::credentials::CredentialPurpose;
use wanaku_types::registry::{
    DEFAULT_NAMESPACE, ForwardEntry, ForwardRegistry, NamespaceEntry, NamespaceRegistry,
    PromptRegistry, ResourceRegistry, ToolRegistry, validate_namespace_name,
};

const LABEL_PREFIX: &str = "rhaf.lifecycle/";
const OWNER: &str = "rhaf.lifecycle/forward";
const BASE: &str = "rhaf.lifecycle/namespace-base";
const ID: &str = "rhaf.lifecycle/registration-id";
const STATE: &str = "rhaf.lifecycle/state";
const REGISTERING: &str = "registering";
const ACTIVE: &str = "active";
const FAILED: &str = "failedRegistrationCleanup";

#[derive(Clone)]
pub struct Lifecycle {
    registry: InMemoryRegistry,
    commit_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    forward: String,
    base: String,
    id: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    #[error("namespaceBase is invalid")]
    InvalidBase,
    #[error("forward name is invalid")]
    InvalidForward,
    #[error("labels contain a reserved lifecycle key")]
    ReservedLabel,
    #[error("forward or namespace ownership conflict")]
    Conflict,
    #[error("cannot write lifecycle metadata: {0}")]
    Registry(String),
}

impl Lifecycle {
    pub fn new(registry: InMemoryRegistry) -> Self {
        Self {
            registry,
            commit_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Serialize registry checks and writes without holding the lock during discovery.
    pub async fn commit<T>(&self, action: impl FnOnce(&InMemoryRegistry) -> T) -> T {
        let _guard = self.commit_guard().await;
        action(&self.registry)
    }

    pub async fn commit_guard(&self) -> MutexGuard<'_, ()> {
        self.commit_lock.lock().await
    }

    pub const fn registry(&self) -> &InMemoryRegistry {
        &self.registry
    }

    /// Reserve a forward and its namespace pair before contacting the upstream.
    pub async fn reserve(&self, forward: &str, base: &str) -> Result<Registration, LifecycleError> {
        if forward.is_empty() {
            return Err(LifecycleError::InvalidForward);
        }
        if base.len() > 51 || validate_namespace_name(base).is_err() {
            return Err(LifecycleError::InvalidBase);
        }
        let registration = Registration {
            forward: forward.to_owned(),
            base: base.to_owned(),
            id: Uuid::new_v4().to_string(),
        };
        self.commit(|registry| registration.reserve(registry))
            .await?;
        Ok(registration)
    }

    /// Recover the registration identity from its persisted namespace ownership.
    pub async fn registration(
        &self,
        forward: &str,
    ) -> Result<Option<Registration>, LifecycleError> {
        self.commit(|registry| Registration::lookup(registry, forward))
            .await
    }

    /// Mark the namespace pair active after the guarded initial publication.
    pub async fn activate(&self, registration: &Registration) -> Result<(), LifecycleError> {
        self.commit(|registry| {
            let forward = registry
                .get_forward(&registration.forward)
                .ok_or(LifecycleError::Conflict)?;
            if !registration.labels_match(&forward.labels) {
                return Err(LifecycleError::Conflict);
            }
            validate_discovery(registry, &forward)?;
            registration.mark_active(registry)
        })
        .await
    }
}

impl Registration {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn conventional_namespace(&self) -> String {
        format!("{}-all", self.base)
    }

    pub fn progressive_namespace(&self) -> String {
        format!("{}-progressive", self.base)
    }

    pub fn mark_active(&self, registry: &InMemoryRegistry) -> Result<(), LifecycleError> {
        self.set_state(registry, ACTIVE)
    }

    pub fn mark_failed(&self, registry: &InMemoryRegistry) -> Result<(), LifecycleError> {
        self.set_state(registry, FAILED)
    }

    fn set_state(&self, registry: &InMemoryRegistry, state: &str) -> Result<(), LifecycleError> {
        for mut namespace in self.owned_namespaces(registry)? {
            namespace.labels.insert(STATE.to_owned(), state.to_owned());
            registry
                .try_register_namespace(namespace)
                .map_err(LifecycleError::Registry)?;
        }
        Ok(())
    }

    pub fn views(registry: &InMemoryRegistry) -> Result<Vec<Forward>, LifecycleError> {
        let namespaces = registry.list_namespaces();
        let names: BTreeSet<_> = namespaces.iter().filter_map(reservation_owner).collect();
        names
            .into_iter()
            .map(|name| {
                Self::lookup(registry, name)?
                    .ok_or(LifecycleError::Conflict)?
                    .view(registry)
            })
            .collect()
    }

    pub fn view(&self, registry: &InMemoryRegistry) -> Result<Forward, LifecycleError> {
        let [conventional, progressive] = self.owned_namespaces(registry)?;
        let state = match label(&conventional.labels, STATE) {
            Some(ACTIVE) => LifecycleState::Active,
            Some(REGISTERING) => LifecycleState::Registering,
            Some(FAILED) => LifecycleState::FailedRegistrationCleanup,
            Some("deleting") => LifecycleState::Deleting,
            _ => return Err(LifecycleError::Conflict),
        };
        let mut view = Forward {
            name: self.forward.clone(),
            address: String::new(),
            namespace_base: self.base.clone(),
            conventional_namespace: conventional.name,
            progressive_namespace: progressive.name,
            registration_id: self.id.clone(),
            labels: Default::default(),
            credential_bindings: CredentialBindings::default(),
            state,
            available: None,
            message: None,
        };
        if let Some(forward) = registry.get_forward(&self.forward) {
            fill_forward_view(&mut view, forward);
        }
        Ok(view)
    }

    /// Stamp the same identity on the candidate for initial publication or replacement.
    pub fn apply_to_forward(&self, forward: &mut ForwardEntry) -> Result<(), LifecycleError> {
        if forward.name != self.forward {
            return Err(LifecycleError::InvalidForward);
        }
        if forward.labels.keys().any(|label| is_lifecycle_label(label)) {
            return Err(LifecycleError::ReservedLabel);
        }
        forward.namespace = Some(self.conventional_namespace());
        forward.labels.insert(BASE.to_owned(), self.base.clone());
        forward.labels.insert(ID.to_owned(), self.id.clone());
        Ok(())
    }

    fn reserve(&self, registry: &InMemoryRegistry) -> Result<(), LifecycleError> {
        let conventional = self.conventional_namespace();
        let progressive = self.progressive_namespace();
        if namespace_is_occupied(registry, &conventional)
            || namespace_is_occupied(registry, &progressive)
            || self.conflicts_with_reservation(registry)
        {
            return Err(LifecycleError::Conflict);
        }
        for name in [conventional, progressive] {
            let labels = HashMap::from([
                (OWNER.to_owned(), self.forward.clone()),
                (BASE.to_owned(), self.base.clone()),
                (ID.to_owned(), self.id.clone()),
                (STATE.to_owned(), REGISTERING.to_owned()),
            ]);
            registry
                .try_register_namespace(NamespaceEntry {
                    name,
                    labels,
                    members: None,
                    auth_required: None,
                    audience: None,
                })
                .map_err(LifecycleError::Registry)?;
        }
        Ok(())
    }

    fn conflicts_with_reservation(&self, registry: &InMemoryRegistry) -> bool {
        let conflicts = |labels: &HashMap<String, String>| {
            label(labels, BASE) == Some(self.base.as_str())
                || label(labels, ID) == Some(self.id.as_str())
        };
        registry.get_forward(&self.forward).is_some()
            || registry.list_namespaces().iter().any(|namespace| {
                reservation_owner(namespace) == Some(self.forward.as_str())
                    || conflicts(&namespace.labels)
            })
            || registry
                .list_forwards()
                .iter()
                .any(|forward| conflicts(&forward.labels))
    }

    pub fn lookup(
        registry: &InMemoryRegistry,
        forward: &str,
    ) -> Result<Option<Self>, LifecycleError> {
        let Some(namespace) = registry
            .list_namespaces()
            .into_iter()
            .find(|namespace| reservation_owner(namespace) == Some(forward))
        else {
            return if protects_forward(registry, forward) {
                Err(LifecycleError::Conflict)
            } else {
                Ok(None)
            };
        };
        let registration =
            Self::from_labels(forward, &namespace.labels).ok_or(LifecycleError::Conflict)?;
        registration.owned_namespaces(registry)?;
        if registry.get_forward(forward).is_some_and(|current| {
            !registration.labels_match(&current.labels)
                || current.namespace.as_deref()
                    != Some(registration.conventional_namespace().as_str())
        }) {
            return Err(LifecycleError::Conflict);
        }
        Ok(Some(registration))
    }

    fn from_labels(forward: &str, labels: &HashMap<String, String>) -> Option<Self> {
        let id = label(labels, ID).filter(|value| !value.is_empty())?;
        let base = label(labels, BASE)?;
        Some(Self {
            forward: forward.to_owned(),
            base: base.to_owned(),
            id: id.to_owned(),
        })
    }

    fn labels_match(&self, labels: &HashMap<String, String>) -> bool {
        label(labels, BASE) == Some(self.base.as_str())
            && label(labels, ID) == Some(self.id.as_str())
    }

    fn owned_namespaces(
        &self,
        registry: &InMemoryRegistry,
    ) -> Result<[NamespaceEntry; 2], LifecycleError> {
        let names = [self.conventional_namespace(), self.progressive_namespace()];
        let get_owned = |name| {
            registry
                .get_namespace(name)
                .filter(|namespace| {
                    !namespace.is_virtual()
                        && reservation_owner(namespace) == Some(self.forward.as_str())
                        && self.labels_match(&namespace.labels)
                })
                .ok_or(LifecycleError::Conflict)
        };
        Ok([get_owned(&names[0])?, get_owned(&names[1])?])
    }
}

fn fill_forward_view(view: &mut Forward, forward: ForwardEntry) {
    view.available = Some(forward.available);
    view.message = forward.status_message.as_ref().map(|_| {
        if forward.available {
            "some upstream capabilities could not be registered"
        } else {
            "upstream discovery failed"
        }
        .to_owned()
    });
    view.credential_bindings = CredentialBindings {
        discovery: forward
            .binding_for(CredentialPurpose::Discovery)
            .map(str::to_owned),
        invocation: forward
            .binding_for(CredentialPurpose::Invocation)
            .map(str::to_owned),
    };
    view.address = forward.address;
    view.labels = forward
        .labels
        .into_iter()
        .filter(|(key, _)| !is_lifecycle_label(key))
        .collect();
}

pub fn is_lifecycle_label(label: &str) -> bool {
    label.starts_with(LABEL_PREFIX)
}

pub fn reservation_owner(namespace: &NamespaceEntry) -> Option<&str> {
    label(&namespace.labels, OWNER)
}

pub fn reservation_base(namespace: &NamespaceEntry) -> Option<&str> {
    label(&namespace.labels, BASE)
}

fn namespace_is_occupied(registry: &InMemoryRegistry, name: &str) -> bool {
    registry.get_namespace(name).is_some()
        || registry
            .list_tools()
            .iter()
            .any(|entry| entry.namespace.as_deref() == Some(name))
        || registry
            .list_resources()
            .iter()
            .any(|entry| entry.namespace.as_deref() == Some(name))
        || registry
            .list_prompts()
            .iter()
            .any(|entry| entry.namespace.as_deref() == Some(name))
        || registry
            .list_agents()
            .iter()
            .any(|agent| agent.namespace == name)
        || registry
            .list_forwards()
            .iter()
            .any(|forward| forward.namespace.as_deref() == Some(name))
}

/// Include namespace-only claims left by an in-progress registration or cleanup.
pub fn protects_forward(registry: &InMemoryRegistry, name: &str) -> bool {
    registry
        .get_forward(name)
        .is_some_and(|entry| entry.labels.keys().any(|label| is_lifecycle_label(label)))
        || registry
            .list_namespaces()
            .iter()
            .any(|namespace| reservation_owner(namespace) == Some(name))
}

/// Include either derived name when only one of the namespace records survives.
pub fn protects_namespace(registry: &InMemoryRegistry, name: &str) -> bool {
    registry
        .get_namespace(name)
        .is_some_and(|entry| entry.labels.keys().any(|label| is_lifecycle_label(label)))
        || [name.strip_suffix("-all"), name.strip_suffix("-progressive")]
            .into_iter()
            .flatten()
            .any(|base| {
                registry
                    .list_namespaces()
                    .iter()
                    .any(|namespace| label(&namespace.labels, BASE) == Some(base))
                    || registry
                        .list_forwards()
                        .iter()
                        .any(|forward| label(&forward.labels, BASE) == Some(base))
            })
}

/// Revalidate the ownership and lifecycle state at the guarded publication point.
pub fn validate_discovery(
    registry: &InMemoryRegistry,
    target: &ForwardEntry,
) -> Result<(), LifecycleError> {
    validate_publication(registry, target, true)
}

/// Refresh can publish only against an active registration or unmanaged forward.
pub fn validate_refresh(
    registry: &InMemoryRegistry,
    target: &ForwardEntry,
) -> Result<(), LifecycleError> {
    validate_publication(registry, target, false)
}

fn validate_publication(
    registry: &InMemoryRegistry,
    target: &ForwardEntry,
    initial: bool,
) -> Result<(), LifecycleError> {
    let namespace = target.namespace.as_deref().unwrap_or(DEFAULT_NAMESPACE);
    let Some(registration) = Registration::from_labels(&target.name, &target.labels) else {
        return if target.labels.keys().any(|label| is_lifecycle_label(label))
            || protects_forward(registry, &target.name)
            || protects_namespace(registry, namespace)
        {
            Err(LifecycleError::Conflict)
        } else {
            Ok(())
        };
    };
    if namespace != registration.conventional_namespace() {
        return Err(LifecycleError::Conflict);
    }
    let [conventional, progressive] = registration.owned_namespaces(registry)?;
    let state = label(&conventional.labels, STATE);
    let allowed_state = state == Some(ACTIVE) || (initial && state == Some(REGISTERING));
    let same_registration = registry
        .get_forward(&target.name)
        .is_none_or(|current| registration.labels_match(&current.labels));
    (state == label(&progressive.labels, STATE) && allowed_state && same_registration)
        .then_some(())
        .ok_or(LifecycleError::Conflict)
}

fn label<'a>(labels: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    labels.get(key).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Context, Result};
    use wanaku_infra::persistence::FilePersistence;

    use super::{
        ID, Lifecycle, LifecycleError, Registration, STATE, label, protects_forward,
        protects_namespace, reservation_owner, validate_discovery, validate_refresh,
    };
    use wanaku_infra::registry::{
        DiscoveredForward, ForwardCapabilities, ForwardPublicationError, InMemoryRegistry,
    };
    use wanaku_types::registry::{
        ForwardEntry, ForwardRegistry, NamespaceEntry, NamespaceRegistry, ToolRegistry,
    };

    fn target(name: &str) -> Result<ForwardEntry> {
        Ok(serde_json::from_value(serde_json::json!({
            "name": name,
            "address": "https://example.test/mcp"
        }))?)
    }

    #[tokio::test]
    async fn conflicts_leave_empty_unowned_namespaces_untouched() -> Result<()> {
        for suffix in ["all", "progressive"] {
            let registry = InMemoryRegistry::new();
            let owner = format!("base-{suffix}");
            registry
                .try_register_namespace(NamespaceEntry {
                    name: owner.clone(),
                    members: None,
                    labels: Default::default(),
                    auth_required: Some(true),
                    audience: None,
                })
                .map_err(anyhow::Error::msg)?;
            let lifecycle = Lifecycle::new(registry.clone());
            assert_eq!(
                lifecycle.reserve("forward", "base").await,
                Err(LifecycleError::Conflict)
            );
            let namespace = registry
                .get_namespace(&owner)
                .context("unowned namespace")?;
            assert_eq!(namespace.auth_required, Some(true));
            assert!(namespace.labels.is_empty());
            let other = if suffix == "all" {
                "base-progressive"
            } else {
                "base-all"
            };
            assert!(registry.get_namespace(other).is_none());
        }
        Ok(())
    }

    #[tokio::test]
    async fn disabled_tool_occupies_an_implicit_namespace() -> Result<()> {
        let registry = InMemoryRegistry::new();
        registry.register_tool(serde_json::from_value(serde_json::json!({
            "name": "existing", "description": "existing tool",
            "type": "mcp-forward", "uri": "https://example.test/mcp",
            "inputSchema": {}, "namespace": "base-progressive", "enabled": false
        }))?);
        assert!(registry.get_namespace("base-progressive").is_none());
        let lifecycle = Lifecycle::new(registry.clone());
        assert_eq!(
            lifecycle.reserve("forward", "base").await,
            Err(LifecycleError::Conflict)
        );
        assert!(registry.get_namespace("base-all").is_none());
        assert!(registry.get_namespace("base-progressive").is_none());
        assert!(
            registry
                .get_tool("base-progressive", "existing")
                .is_some_and(|entry| !entry.enabled)
        );
        Ok(())
    }

    #[tokio::test]
    async fn cloned_coordinators_serialize_name_and_base_claims() -> Result<()> {
        let registry = InMemoryRegistry::new();
        let lifecycle = Lifecycle::new(registry.clone());
        let concurrent = lifecycle.clone();
        let held = lifecycle.commit_guard().await;
        let first_coordinator = lifecycle.clone();
        let first = tokio::spawn(async move { first_coordinator.reserve("one", "base").await });
        let second = tokio::spawn(async move { concurrent.reserve("two", "base").await });
        tokio::task::yield_now().await;
        assert!(!first.is_finished());
        assert!(!second.is_finished());
        drop(held);
        let first = first.await?;
        let second = second.await?;
        assert_ne!(first.is_ok(), second.is_ok());
        let winner = reservation_owner(
            &registry
                .get_namespace("base-all")
                .context("base reservation")?,
        )
        .context("reservation owner")?
        .to_owned();
        assert_eq!(
            lifecycle.reserve(&winner, "other").await,
            Err(LifecycleError::Conflict)
        );
        assert!(registry.get_namespace("other-all").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn validates_namespace_base_boundaries() {
        let lifecycle = Lifecycle::new(InMemoryRegistry::new());
        for base in ["", "-a", "a-", "a_b", "A", "é", &"a".repeat(52)] {
            assert_eq!(
                lifecycle.reserve("forward", base).await,
                Err(LifecycleError::InvalidBase)
            );
        }
        assert!(lifecycle.reserve("short", "1").await.is_ok());
        assert!(lifecycle.reserve("long", &"z".repeat(51)).await.is_ok());
    }

    #[tokio::test]
    async fn persisted_namespaces_retain_reservations_without_a_forward() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let backend = Arc::new(FilePersistence::new(dir.path().join("registry.json")));
        let registry = InMemoryRegistry::with_persistence(backend.clone());
        let lifecycle = Lifecycle::new(registry.clone());
        let reservation = lifecycle.reserve("forward", "base").await?;
        registry.flush_persistence(Duration::from_secs(2))?;
        let restored = InMemoryRegistry::with_persistence(backend);
        restored.load_persisted();
        let namespace = restored
            .get_namespace("base-progressive")
            .context("namespace")?;
        assert_eq!(label(&namespace.labels, ID), Some(reservation.id()));
        assert_eq!(reservation_owner(&namespace), Some("forward"));
        assert!(protects_forward(&restored, "forward"));
        assert!(protects_namespace(&restored, "base-all"));
        let restarted = Lifecycle::new(restored.clone());
        assert_eq!(restarted.registration("forward").await?, Some(reservation));
        assert!(restored.remove_namespace("base-all"));
        assert!(protects_namespace(&restored, "base-all"));
        for (name, base) in [("another", "base"), ("forward", "another")] {
            assert_eq!(
                restarted.reserve(name, base).await,
                Err(LifecycleError::Conflict)
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn refuses_duplicate_registration_identity() -> Result<()> {
        let registry = InMemoryRegistry::new();
        let lifecycle = Lifecycle::new(registry.clone());
        let registration = lifecycle.reserve("forward", "base").await?;
        let other = lifecycle.reserve("other", "other").await?;
        assert_ne!(registration.id(), other.id());
        let duplicate = Registration {
            forward: "duplicate".to_owned(),
            base: "unused".to_owned(),
            id: registration.id().to_owned(),
        };
        assert_eq!(
            lifecycle
                .commit(|registry| duplicate.reserve(registry))
                .await,
            Err(LifecycleError::Conflict)
        );
        assert!(registry.get_namespace("unused-all").is_none());
        Ok(())
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "keep the publication order and persistence assertions in one race scenario"
    )]
    fn stale_reconnect_failure_preserves_newer_refresh_after_reload() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let backend = Arc::new(FilePersistence::new(dir.path().join("registry.json")));
        let registry = InMemoryRegistry::with_persistence(backend.clone());
        let name = "forward";
        let mut forward = target(name)?;
        forward.namespace = Some("base-all".to_owned());
        forward.available = false;
        registry.register_forward(forward);

        let current = registry
            .begin_forward_refresh(name)
            .context("current probe")?;
        registry.publish_forward_discovery(current, Err("current failure".to_owned()))?;
        let reconnect = registry
            .begin_forward_refresh(name)
            .context("reconnect probe")?;
        assert!(!reconnect.target().available);
        assert_eq!(
            reconnect.target().status_message.as_deref(),
            Some("current failure")
        );

        // Hold the failed reconnect's request until the manual refresh publishes.
        let manual = registry
            .begin_forward_refresh(name)
            .context("manual refresh")?;
        let refreshed = registry.publish_forward_discovery(
            manual,
            Ok(DiscoveredForward {
                server_info: None,
                capabilities: ForwardCapabilities {
                    tools: Vec::new(),
                    resources: Vec::new(),
                    prompts: Vec::new(),
                },
            }),
        )?;
        assert_eq!(refreshed.forward.address, reconnect.target().address);
        assert_eq!(refreshed.forward.namespace, reconnect.target().namespace);
        assert_eq!(
            registry
                .publish_forward_discovery(reconnect, Err("stale failure".to_owned()))
                .map(|_| ()),
            Err(ForwardPublicationError::Stale(name.to_owned())),
        );

        registry.flush_persistence(Duration::from_secs(2))?;
        let restored = InMemoryRegistry::with_persistence(backend);
        restored.load_persisted();
        for registry in [registry, restored] {
            let forward = registry.get_forward(name).context("refreshed forward")?;
            assert!(forward.available);
            assert_eq!(forward.status_message, None);
        }
        Ok(())
    }

    async fn publish_initial(lifecycle: &Lifecycle, candidate: ForwardEntry) -> Result<()> {
        let request = lifecycle
            .registry()
            .begin_forward_registration(candidate, false)?;
        lifecycle
            .commit(|registry| -> Result<()> {
                validate_discovery(registry, request.target())?;
                registry.publish_forward_discovery(
                    request,
                    Ok(DiscoveredForward {
                        server_info: None,
                        capabilities: ForwardCapabilities {
                            tools: Vec::new(),
                            resources: Vec::new(),
                            prompts: Vec::new(),
                        },
                    }),
                )?;
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn publication_checks_identity_and_lifecycle_state() -> Result<()> {
        let registry = InMemoryRegistry::new();
        let lifecycle = Lifecycle::new(registry.clone());
        let registration = lifecycle.reserve("forward", "base").await?;
        let mut candidate = target("forward")?;
        candidate.namespace = Some(registration.conventional_namespace());
        assert_eq!(
            validate_discovery(&registry, &candidate),
            Err(LifecycleError::Conflict)
        );
        registration.apply_to_forward(&mut candidate)?;
        assert!(validate_discovery(&registry, &candidate).is_ok());
        assert_eq!(
            validate_refresh(&registry, &candidate),
            Err(LifecycleError::Conflict)
        );
        publish_initial(&lifecycle, candidate.clone()).await?;
        lifecycle.activate(&registration).await?;
        assert!(validate_refresh(&registry, &candidate).is_ok());
        let mut replacement = target("forward")?;
        registration.apply_to_forward(&mut replacement)?;
        assert_eq!(label(&replacement.labels, ID), Some(registration.id()));
        assert!(validate_discovery(&registry, &replacement).is_ok());
        replacement.labels.insert(ID.to_owned(), "stale".to_owned());
        assert_eq!(
            validate_discovery(&registry, &replacement),
            Err(LifecycleError::Conflict)
        );
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_state_still_holds_namespace_reservation() -> Result<()> {
        let registry = InMemoryRegistry::new();
        let lifecycle = Lifecycle::new(registry.clone());
        let registration = lifecycle.reserve("forward", "base").await?;
        let mut candidate = target("forward")?;
        registration.apply_to_forward(&mut candidate)?;
        publish_initial(&lifecycle, candidate.clone()).await?;
        lifecycle.activate(&registration).await?;
        let mut progressive = registry
            .get_namespace("base-progressive")
            .context("progressive namespace")?;
        progressive
            .labels
            .insert(STATE.to_owned(), "deleting".to_owned());
        registry
            .try_register_namespace(progressive)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            validate_refresh(&registry, &candidate),
            Err(LifecycleError::Conflict)
        );
        assert_eq!(
            lifecycle.reserve("new", "base").await,
            Err(LifecycleError::Conflict)
        );
        Ok(())
    }
}
