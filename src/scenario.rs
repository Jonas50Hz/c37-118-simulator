//! Startup-validated, frame-relative fault-scenario catalogs.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::Path,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{config::MAX_LOGICAL_PMUS, identity::sha256_hex};

pub const SCENARIO_CATALOG_VERSION: u32 = 1;
/// Maximum immutable catalog entries accepted by the bounded management response.
pub const MAX_SCENARIO_CATALOG_ENTRIES: usize = 128;
/// Maximum distinct endpoint and PDC targets retained by the 150-PMU fleet read model.
pub const MAX_SCENARIO_TARGETS: usize = MAX_LOGICAL_PMUS * 3;
/// Maximum UTF-8 bytes retained verbatim for an operator attribution label.
///
/// This bound keeps a 48-record console page below the management response cap
/// without changing the meaning of absent or empty labels.
pub const MAX_SCENARIO_ACTOR_LABEL_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScenarioCatalog {
    pub version: u32,
    scenarios: Vec<ScenarioDefinition>,
}

impl ScenarioCatalog {
    pub fn scenarios(&self) -> &[ScenarioDefinition] {
        &self.scenarios
    }

    pub fn scenario(&self, name: &str) -> Option<&ScenarioDefinition> {
        self.scenarios.iter().find(|scenario| scenario.name == name)
    }

    /// Returns a stable SHA-256 identity of the compiled catalog content.
    pub fn content_sha256(&self) -> String {
        let contents = serde_json::to_vec(self)
            .expect("compiled scenario catalogs must serialize as JSON");
        sha256_hex(&contents)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScenarioDefinition {
    pub name: String,
    pub kind: ScenarioKind,
    pub start_frame_offset: u64,
    pub lifecycle: ScenarioLifecycle,
    pub duration_frames: Option<u64>,
    pub signal: Option<SignalExcursion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioKind {
    Normal,
    DegradedTime,
    MissingFrames,
    DisconnectPdc,
    SignalExcursion,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioLifecycle {
    Transient,
    Sustained,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SignalExcursion {
    pub voltage_magnitude_delta: f32,
    pub frequency_deviation_hz: f32,
    pub rocof_hz_per_s: f32,
}

/// The lifetime of a prepared runtime scenario activation token.
pub const ACTIVATION_TOKEN_TTL: Duration = Duration::from_secs(60);

/// A single simulator endpoint or one PDC connection attached to an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioTarget {
    Endpoint { stream_id: u16 },
    Pdc { stream_id: u16, connection_id: u64 },
}

impl ScenarioTarget {
    pub fn stream_id(self) -> u16 {
        match self {
            Self::Endpoint { stream_id } | Self::Pdc { stream_id, .. } => stream_id,
        }
    }
}

/// An opaque token returned by prepare and consumed by confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActivationToken(u64);

impl ActivationToken {
    pub fn value(self) -> u64 {
        self.0
    }

    pub fn from_value(value: u64) -> Option<Self> {
        (value != 0).then_some(Self(value))
    }
}

impl Serialize for ActivationToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(&self.0)
    }
}

/// The operator action represented by a prepared or confirmed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioAction {
    Activate { scenario_name: String },
    Clear,
}

/// A prepared action that still requires confirmation.
///
/// The token deadline stays internal so this snapshot can be safely exposed by
/// a future management API without leaking `Instant` values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreparedScenarioSnapshot {
    pub token: ActivationToken,
    pub confirm_expires_in_ms: u64,
    pub target: ScenarioTarget,
    pub action: ScenarioAction,
    pub actor_label: Option<String>,
}

/// A confirmed action waiting for its first reporting boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingScenarioSnapshot {
    pub target: ScenarioTarget,
    pub action: ScenarioAction,
    pub actor_label: Option<String>,
}

/// A scenario that has crossed its first eligible reporting boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveScenarioSnapshot {
    pub target: ScenarioTarget,
    pub scenario_name: String,
    pub kind: ScenarioKind,
    pub lifecycle: ScenarioLifecycle,
    pub start_frame_offset: u64,
    pub first_eligible_boundary: u64,
    pub actor_label: Option<String>,
}

/// A management-safe view of controller state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScenarioControllerSnapshot {
    pub current_sample_index: Option<u64>,
    pub prepared: Vec<PreparedScenarioSnapshot>,
    pub pending: Vec<PendingScenarioSnapshot>,
    pub active: Vec<ActiveScenarioSnapshot>,
}

/// Wire-independent effects to apply while emitting one endpoint's frame.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct FramePlan {
    pub degraded_time: bool,
    pub omit_data: bool,
    pub signal: Option<SignalExcursion>,
    pub disconnect_pdc_connection_ids: Vec<u64>,
}

impl FramePlan {
    pub fn is_noop(&self) -> bool {
        !self.degraded_time
            && !self.omit_data
            && self.signal.is_none()
            && self.disconnect_pdc_connection_ids.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioControllerError {
    InvalidTarget { target: ScenarioTarget },
    InvalidScenarioName { name: String },
    InvalidActorLabel { maximum: usize },
    UnknownScenario { name: String },
    IncompatibleTarget {
        target: ScenarioTarget,
        kind: ScenarioKind,
    },
    TargetBusy { target: ScenarioTarget },
    TargetCapacityExceeded { maximum: usize },
    NoActiveScenario { target: ScenarioTarget },
    ClearRequiresSustainedScenario { target: ScenarioTarget },
    UnknownToken { token: ActivationToken },
    ExpiredToken { token: ActivationToken },
    TokenSpaceExhausted,
    TokenExpiryOverflow,
    NonMonotonicBoundary { previous: u64, received: u64 },
}

impl fmt::Display for ScenarioControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget { target } => {
                write!(formatter, "scenario target has an invalid stream ID: {target:?}")
            }
            Self::InvalidScenarioName { name } => write!(
                formatter,
                "scenario name {name:?} must contain 1 to 64 lowercase ASCII letters, digits, or hyphens"
            ),
            Self::InvalidActorLabel { maximum } => write!(
                formatter,
                "actor label must contain at most {maximum} UTF-8 bytes"
            ),
            Self::UnknownScenario { name } => write!(formatter, "unknown scenario {name:?}"),
            Self::IncompatibleTarget { target, kind } => write!(
                formatter,
                "scenario kind {kind:?} cannot be applied to target {target:?}"
            ),
            Self::TargetBusy { target } => {
                write!(formatter, "scenario target already has an active or pending action: {target:?}")
            }
            Self::TargetCapacityExceeded { maximum } => write!(
                formatter,
                "scenario controller cannot track more than {maximum} targets"
            ),
            Self::NoActiveScenario { target } => {
                write!(formatter, "scenario target has no active scenario to clear: {target:?}")
            }
            Self::ClearRequiresSustainedScenario { target } => write!(
                formatter,
                "only an active sustained scenario can be cleared: {target:?}"
            ),
            Self::UnknownToken { token } => {
                write!(formatter, "unknown or already consumed activation token {}", token.value())
            }
            Self::ExpiredToken { token } => {
                write!(formatter, "expired activation token {}", token.value())
            }
            Self::TokenSpaceExhausted => formatter.write_str("activation token space is exhausted"),
            Self::TokenExpiryOverflow => {
                formatter.write_str("activation token expiry cannot be represented")
            }
            Self::NonMonotonicBoundary { previous, received } => write!(
                formatter,
                "reporting boundaries must increase: received {received} after {previous}"
            ),
        }
    }
}

impl std::error::Error for ScenarioControllerError {}

#[derive(Debug, Clone)]
enum StoredScenarioAction {
    Activate(ScenarioDefinition),
    Clear,
}

impl StoredScenarioAction {
    fn snapshot(&self) -> ScenarioAction {
        match self {
            Self::Activate(scenario) => ScenarioAction::Activate {
                scenario_name: scenario.name.clone(),
            },
            Self::Clear => ScenarioAction::Clear,
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedScenarioAction {
    target: ScenarioTarget,
    action: StoredScenarioAction,
    actor_label: Option<String>,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct PendingScenarioAction {
    action: StoredScenarioAction,
    actor_label: Option<String>,
}

#[derive(Debug, Clone)]
struct ActiveScenarioAction {
    scenario: ScenarioDefinition,
    actor_label: Option<String>,
    first_eligible_boundary: u64,
}

/// Deterministic runtime scenario state, driven only by caller-supplied time
/// and reporting-boundary indices.
#[derive(Debug, Clone)]
pub struct ScenarioController {
    catalog: ScenarioCatalog,
    prepared: BTreeMap<ActivationToken, PreparedScenarioAction>,
    prepared_targets: BTreeMap<ScenarioTarget, ActivationToken>,
    pending: BTreeMap<ScenarioTarget, PendingScenarioAction>,
    active: BTreeMap<ScenarioTarget, ActiveScenarioAction>,
    current_sample_index: Option<u64>,
    next_token: u64,
    revision: u64,
}

impl ScenarioController {
    pub fn new(catalog: ScenarioCatalog) -> Self {
        Self {
            catalog,
            prepared: BTreeMap::new(),
            prepared_targets: BTreeMap::new(),
            pending: BTreeMap::new(),
            active: BTreeMap::new(),
            current_sample_index: None,
            next_token: 1,
            revision: 0,
        }
    }

    pub fn catalog(&self) -> &ScenarioCatalog {
        &self.catalog
    }

    /// Monotonically increases whenever lifecycle records change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn has_active_target(&self, target: ScenarioTarget) -> bool {
        self.active.contains_key(&target)
    }

    /// Prepares activation of a catalog scenario without affecting frame plans.
    pub fn prepare_activation(
        &mut self,
        target: ScenarioTarget,
        scenario_name: impl AsRef<str>,
        actor_label: Option<String>,
        now: Instant,
    ) -> Result<PreparedScenarioSnapshot, ScenarioControllerError> {
        let scenario_name = scenario_name.as_ref();
        validate_controller_target(target)?;
        validate_controller_name(scenario_name)?;
        validate_actor_label(actor_label.as_deref())?;

        let scenario = self
            .catalog
            .scenario(scenario_name)
            .cloned()
            .ok_or_else(|| ScenarioControllerError::UnknownScenario {
                name: scenario_name.to_owned(),
            })?;
        validate_scenario_target(target, scenario.kind)?;

        self.prepare_action(
            target,
            StoredScenarioAction::Activate(scenario),
            actor_label,
            now,
            false,
        )
    }

    /// Prepares a confirmed clear for an active sustained scenario.
    pub fn prepare_clear(
        &mut self,
        target: ScenarioTarget,
        actor_label: Option<String>,
        now: Instant,
    ) -> Result<PreparedScenarioSnapshot, ScenarioControllerError> {
        validate_controller_target(target)?;
        validate_actor_label(actor_label.as_deref())?;
        self.discard_expired_preparations(now);

        if self.prepared_targets.contains_key(&target) || self.pending.contains_key(&target) {
            return Err(ScenarioControllerError::TargetBusy { target });
        }

        let active = self
            .active
            .get(&target)
            .ok_or(ScenarioControllerError::NoActiveScenario { target })?;
        if active.scenario.lifecycle != ScenarioLifecycle::Sustained {
            return Err(ScenarioControllerError::ClearRequiresSustainedScenario { target });
        }

        self.prepare_action(target, StoredScenarioAction::Clear, actor_label, now, true)
    }

    /// Consumes a prepared token and queues the action for the next boundary.
    pub fn confirm(
        &mut self,
        token: ActivationToken,
        now: Instant,
    ) -> Result<PendingScenarioSnapshot, ScenarioControllerError> {
        let prepared = self
            .prepared
            .remove(&token)
            .ok_or(ScenarioControllerError::UnknownToken { token })?;
        self.prepared_targets.remove(&prepared.target);
        self.bump_revision();

        if now >= prepared.expires_at {
            return Err(ScenarioControllerError::ExpiredToken { token });
        }
        self.discard_expired_preparations(now);

        let target = prepared.target;
        match &prepared.action {
            StoredScenarioAction::Activate(_) => {
                if self.pending.contains_key(&target) || self.active.contains_key(&target) {
                    return Err(ScenarioControllerError::TargetBusy { target });
                }
            }
            StoredScenarioAction::Clear => {
                if self.pending.contains_key(&target) {
                    return Err(ScenarioControllerError::TargetBusy { target });
                }
                let active = self
                    .active
                    .get(&target)
                    .ok_or(ScenarioControllerError::NoActiveScenario { target })?;
                if active.scenario.lifecycle != ScenarioLifecycle::Sustained {
                    return Err(ScenarioControllerError::ClearRequiresSustainedScenario { target });
                }
            }
        }

        let snapshot = PendingScenarioSnapshot {
            target,
            action: prepared.action.snapshot(),
            actor_label: prepared.actor_label.clone(),
        };
        self.pending.insert(
            target,
            PendingScenarioAction {
                action: prepared.action,
                actor_label: prepared.actor_label,
            },
        );
        Ok(snapshot)
    }

    /// Consumes a live prepared token without queueing an action.
    pub fn cancel(
        &mut self,
        token: ActivationToken,
        now: Instant,
    ) -> Result<PreparedScenarioSnapshot, ScenarioControllerError> {
        let prepared = self
            .prepared
            .remove(&token)
            .ok_or(ScenarioControllerError::UnknownToken { token })?;
        self.prepared_targets.remove(&prepared.target);
        self.bump_revision();

        if now >= prepared.expires_at {
            return Err(ScenarioControllerError::ExpiredToken { token });
        }
        self.discard_expired_preparations(now);
        Ok(prepared.snapshot(token, now))
    }

    /// Applies queued actions at a reporting boundary and expires transients.
    pub fn advance_boundary(
        &mut self,
        sample_index: u64,
    ) -> Result<(), ScenarioControllerError> {
        if let Some(previous) = self.current_sample_index {
            if sample_index <= previous {
                return Err(ScenarioControllerError::NonMonotonicBoundary {
                    previous,
                    received: sample_index,
                });
            }
        }

        self.current_sample_index = Some(sample_index);
        let pending = std::mem::take(&mut self.pending);
        let changed_lifecycle_records = !pending.is_empty();
        for (target, action) in pending {
            match action.action {
                StoredScenarioAction::Activate(scenario) => {
                    self.active.insert(
                        target,
                        ActiveScenarioAction {
                            scenario,
                            actor_label: action.actor_label,
                            first_eligible_boundary: sample_index,
                        },
                    );
                }
                StoredScenarioAction::Clear => {
                    self.active.remove(&target);
                }
            }
        }
        let active_count = self.active.len();
        self.expire_transient_actions(sample_index);
        if changed_lifecycle_records || self.active.len() != active_count {
            self.bump_revision();
        }
        Ok(())
    }

    /// Returns the effects that apply to one endpoint at the current boundary.
    pub fn frame_plan(&self, stream_id: u16) -> FramePlan {
        let Some(sample_index) = self.current_sample_index else {
            return FramePlan::default();
        };

        let mut plan = FramePlan::default();
        if let Some(active) = self.active.get(&ScenarioTarget::Endpoint { stream_id }) {
            if is_effective_at(active, sample_index) {
                match active.scenario.kind {
                    ScenarioKind::DegradedTime => plan.degraded_time = true,
                    ScenarioKind::MissingFrames => plan.omit_data = true,
                    ScenarioKind::SignalExcursion => plan.signal = active.scenario.signal,
                    ScenarioKind::Normal | ScenarioKind::Recovery | ScenarioKind::DisconnectPdc => {}
                }
            }
        }

        for (target, active) in &self.active {
            let ScenarioTarget::Pdc {
                stream_id: target_stream_id,
                connection_id,
            } = target
            else {
                continue;
            };
            if *target_stream_id == stream_id
                && active.scenario.kind == ScenarioKind::DisconnectPdc
                && is_effective_at(active, sample_index)
            {
                plan.disconnect_pdc_connection_ids.push(*connection_id);
            }
        }

        plan
    }

    pub fn snapshot(&mut self, now: Instant) -> ScenarioControllerSnapshot {
        self.discard_expired_preparations(now);
        ScenarioControllerSnapshot {
            current_sample_index: self.current_sample_index,
            prepared: self.prepared_actions_at(now),
            pending: self.pending_actions(),
            active: self.active_scenarios(),
        }
    }

    pub fn prepared_actions(&self) -> Vec<PreparedScenarioSnapshot> {
        self.prepared_actions_at(Instant::now())
    }

    fn prepared_actions_at(&self, now: Instant) -> Vec<PreparedScenarioSnapshot> {
        self.prepared
            .iter()
            .map(|(token, prepared)| prepared.snapshot(*token, now))
            .collect()
    }

    pub fn pending_actions(&self) -> Vec<PendingScenarioSnapshot> {
        self.pending
            .iter()
            .map(|(target, pending)| PendingScenarioSnapshot {
                target: *target,
                action: pending.action.snapshot(),
                actor_label: pending.actor_label.clone(),
            })
            .collect()
    }

    pub fn active_scenarios(&self) -> Vec<ActiveScenarioSnapshot> {
        self.active
            .iter()
            .map(|(target, active)| ActiveScenarioSnapshot {
                target: *target,
                scenario_name: active.scenario.name.clone(),
                kind: active.scenario.kind,
                lifecycle: active.scenario.lifecycle,
                start_frame_offset: active.scenario.start_frame_offset,
                first_eligible_boundary: active.first_eligible_boundary,
                actor_label: active.actor_label.clone(),
            })
            .collect()
    }

    fn prepare_action(
        &mut self,
        target: ScenarioTarget,
        action: StoredScenarioAction,
        actor_label: Option<String>,
        now: Instant,
        permits_active_target: bool,
    ) -> Result<PreparedScenarioSnapshot, ScenarioControllerError> {
        self.discard_expired_preparations(now);
        if self.prepared_targets.contains_key(&target)
            || self.pending.contains_key(&target)
            || (!permits_active_target && self.active.contains_key(&target))
        {
            return Err(ScenarioControllerError::TargetBusy { target });
        }
        if !self.tracks_target(target) && self.tracked_target_count() >= MAX_SCENARIO_TARGETS {
            return Err(ScenarioControllerError::TargetCapacityExceeded {
                maximum: MAX_SCENARIO_TARGETS,
            });
        }

        let expires_at = now
            .checked_add(ACTIVATION_TOKEN_TTL)
            .ok_or(ScenarioControllerError::TokenExpiryOverflow)?;
        let next_token = self
            .next_token
            .checked_add(1)
            .ok_or(ScenarioControllerError::TokenSpaceExhausted)?;
        let token = ActivationToken(self.next_token);
        self.next_token = next_token;

        let prepared = PreparedScenarioAction {
            target,
            action,
            actor_label,
            expires_at,
        };
        let snapshot = prepared.snapshot(token, now);
        self.prepared.insert(token, prepared);
        self.prepared_targets.insert(target, token);
        self.bump_revision();
        Ok(snapshot)
    }

    fn tracks_target(&self, target: ScenarioTarget) -> bool {
        self.prepared_targets.contains_key(&target)
            || self.pending.contains_key(&target)
            || self.active.contains_key(&target)
    }

    fn tracked_target_count(&self) -> usize {
        self.active
            .keys()
            .chain(self.pending.keys())
            .chain(self.prepared_targets.keys())
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
    }

    fn discard_expired_preparations(&mut self, now: Instant) {
        let expired_tokens: Vec<_> = self
            .prepared
            .iter()
            .filter_map(|(token, prepared)| (now >= prepared.expires_at).then_some(*token))
            .collect();
        for token in expired_tokens {
            if let Some(prepared) = self.prepared.remove(&token) {
                self.prepared_targets.remove(&prepared.target);
                self.bump_revision();
            }
        }
    }

    fn bump_revision(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("scenario controller revision space is exhausted");
    }

    fn expire_transient_actions(&mut self, sample_index: u64) {
        self.active.retain(|_, active| {
            active.scenario.lifecycle != ScenarioLifecycle::Transient
                || !is_expired_at(active, sample_index)
        });
    }
}

impl PreparedScenarioAction {
    fn snapshot(&self, token: ActivationToken, now: Instant) -> PreparedScenarioSnapshot {
        PreparedScenarioSnapshot {
            token,
            confirm_expires_in_ms: confirmation_expires_in_ms(self.expires_at, now),
            target: self.target,
            action: self.action.snapshot(),
            actor_label: self.actor_label.clone(),
        }
    }
}

fn confirmation_expires_in_ms(expires_at: Instant, now: Instant) -> u64 {
    let Some(remaining) = expires_at.checked_duration_since(now) else {
        return 0;
    };
    u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX)
}

fn validate_controller_target(target: ScenarioTarget) -> Result<(), ScenarioControllerError> {
    if target.stream_id() == 0 {
        return Err(ScenarioControllerError::InvalidTarget { target });
    }
    Ok(())
}

fn validate_controller_name(name: &str) -> Result<(), ScenarioControllerError> {
    validate_name(name).map_err(|_| ScenarioControllerError::InvalidScenarioName {
        name: name.to_owned(),
    })
}

pub fn validate_actor_label(actor_label: Option<&str>) -> Result<(), ScenarioControllerError> {
    if actor_label.is_some_and(|label| label.len() > MAX_SCENARIO_ACTOR_LABEL_BYTES) {
        return Err(ScenarioControllerError::InvalidActorLabel {
            maximum: MAX_SCENARIO_ACTOR_LABEL_BYTES,
        });
    }
    Ok(())
}

fn validate_scenario_target(
    target: ScenarioTarget,
    kind: ScenarioKind,
) -> Result<(), ScenarioControllerError> {
    let compatible = matches!(
        (kind, target),
        (ScenarioKind::DisconnectPdc, ScenarioTarget::Pdc { .. })
            | (
                ScenarioKind::Normal
                    | ScenarioKind::DegradedTime
                    | ScenarioKind::MissingFrames
                    | ScenarioKind::SignalExcursion
                    | ScenarioKind::Recovery,
                ScenarioTarget::Endpoint { .. }
            )
    );
    if compatible {
        Ok(())
    } else {
        Err(ScenarioControllerError::IncompatibleTarget { target, kind })
    }
}

fn is_effective_at(active: &ActiveScenarioAction, sample_index: u64) -> bool {
    sample_index
        .checked_sub(active.first_eligible_boundary)
        .is_some_and(|elapsed| elapsed >= active.scenario.start_frame_offset)
}

fn is_expired_at(active: &ActiveScenarioAction, sample_index: u64) -> bool {
    let Some(elapsed) = sample_index.checked_sub(active.first_eligible_boundary) else {
        return false;
    };
    let Some(elapsed_since_start) = elapsed.checked_sub(active.scenario.start_frame_offset) else {
        return false;
    };
    let duration = active
        .scenario
        .duration_frames
        .expect("compiled transient scenarios always have a duration");
    elapsed_since_start >= duration
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioCatalogError(String);

impl ScenarioCatalogError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ScenarioCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ScenarioCatalogError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScenarioCatalog {
    version: u32,
    scenarios: Vec<RawScenario>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScenario {
    name: String,
    kind: RawScenarioKind,
    start_frame_offset: u64,
    lifecycle: RawScenarioLifecycle,
    #[serde(default)]
    duration_frames: Option<u64>,
    #[serde(default)]
    signal: Option<RawSignalExcursion>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawScenarioKind {
    Normal,
    DegradedTime,
    MissingFrames,
    DisconnectPdc,
    SignalExcursion,
    Recovery,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawScenarioLifecycle {
    Transient,
    Sustained,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSignalExcursion {
    voltage_magnitude_delta: f32,
    frequency_deviation_hz: f32,
    rocof_hz_per_s: f32,
}

pub fn load_catalog(path: impl AsRef<Path>) -> Result<ScenarioCatalog, ScenarioCatalogError> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path).map_err(|error| {
        ScenarioCatalogError::new(format!("cannot read {}: {error}", path.display()))
    })?;
    parse_catalog(&contents)
}

pub fn parse_catalog(contents: &str) -> Result<ScenarioCatalog, ScenarioCatalogError> {
    let catalog = serde_yaml::from_str::<RawScenarioCatalog>(contents)
        .map_err(|error| ScenarioCatalogError::new(format!("invalid scenario catalog: {error}")))?;
    compile_catalog(catalog)
}

fn compile_catalog(catalog: RawScenarioCatalog) -> Result<ScenarioCatalog, ScenarioCatalogError> {
    if catalog.version != SCENARIO_CATALOG_VERSION {
        return Err(ScenarioCatalogError::new(format!(
            "scenario catalog version must be {SCENARIO_CATALOG_VERSION}"
        )));
    }
    if catalog.scenarios.is_empty() {
        return Err(ScenarioCatalogError::new(
            "scenario catalog must define at least one scenario",
        ));
    }
    if catalog.scenarios.len() > MAX_SCENARIO_CATALOG_ENTRIES {
        return Err(ScenarioCatalogError::new(format!(
            "scenario catalog must define no more than {MAX_SCENARIO_CATALOG_ENTRIES} scenarios to fit the management response limit"
        )));
    }

    let mut scenarios = Vec::with_capacity(catalog.scenarios.len());
    for raw in catalog.scenarios {
        validate_name(&raw.name)?;
        if scenarios.iter().any(|scenario: &ScenarioDefinition| scenario.name == raw.name) {
            return Err(ScenarioCatalogError::new(
                "scenario catalog must not contain duplicate scenario names",
            ));
        }
        scenarios.push(compile_scenario(raw)?);
    }

    Ok(ScenarioCatalog {
        version: catalog.version,
        scenarios,
    })
}

fn compile_scenario(raw: RawScenario) -> Result<ScenarioDefinition, ScenarioCatalogError> {
    let kind = match raw.kind {
        RawScenarioKind::Normal => ScenarioKind::Normal,
        RawScenarioKind::DegradedTime => ScenarioKind::DegradedTime,
        RawScenarioKind::MissingFrames => ScenarioKind::MissingFrames,
        RawScenarioKind::DisconnectPdc => ScenarioKind::DisconnectPdc,
        RawScenarioKind::SignalExcursion => ScenarioKind::SignalExcursion,
        RawScenarioKind::Recovery => ScenarioKind::Recovery,
    };
    let lifecycle = match raw.lifecycle {
        RawScenarioLifecycle::Transient => ScenarioLifecycle::Transient,
        RawScenarioLifecycle::Sustained => ScenarioLifecycle::Sustained,
    };
    let duration_frames = match (lifecycle, raw.duration_frames) {
        (ScenarioLifecycle::Transient, Some(duration_frames)) if duration_frames > 0 => {
            Some(duration_frames)
        }
        (ScenarioLifecycle::Transient, Some(_)) => {
            return Err(ScenarioCatalogError::new(
                "transient scenarios must have a positive duration_frames value",
            ))
        }
        (ScenarioLifecycle::Transient, None) => {
            return Err(ScenarioCatalogError::new(
                "transient scenarios must define duration_frames",
            ))
        }
        (ScenarioLifecycle::Sustained, None) => None,
        (ScenarioLifecycle::Sustained, Some(_)) => {
            return Err(ScenarioCatalogError::new(
                "sustained scenarios must not define duration_frames",
            ))
        }
    };
    let signal = match (kind, raw.signal) {
        (ScenarioKind::SignalExcursion, Some(signal)) => Some(compile_signal(signal)?),
        (ScenarioKind::SignalExcursion, None) => {
            return Err(ScenarioCatalogError::new(
                "signal_excursion scenarios must define signal values",
            ))
        }
        (_, Some(_)) => {
            return Err(ScenarioCatalogError::new(
                "only signal_excursion scenarios may define signal values",
            ))
        }
        (_, None) => None,
    };

    Ok(ScenarioDefinition {
        name: raw.name,
        kind,
        start_frame_offset: raw.start_frame_offset,
        lifecycle,
        duration_frames,
        signal,
    })
}

fn compile_signal(raw: RawSignalExcursion) -> Result<SignalExcursion, ScenarioCatalogError> {
    let signal = SignalExcursion {
        voltage_magnitude_delta: raw.voltage_magnitude_delta,
        frequency_deviation_hz: raw.frequency_deviation_hz,
        rocof_hz_per_s: raw.rocof_hz_per_s,
    };
    if !signal.voltage_magnitude_delta.is_finite()
        || !signal.frequency_deviation_hz.is_finite()
        || !signal.rocof_hz_per_s.is_finite()
    {
        return Err(ScenarioCatalogError::new(
            "signal excursion values must be finite",
        ));
    }
    if signal.voltage_magnitude_delta == 0.0
        && signal.frequency_deviation_hz == 0.0
        && signal.rocof_hz_per_s == 0.0
    {
        return Err(ScenarioCatalogError::new(
            "signal_excursion must change at least one signal",
        ));
    }
    Ok(signal)
}

fn validate_name(name: &str) -> Result<(), ScenarioCatalogError> {
    if name.is_empty()
        || name.len() > 64
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
        })
    {
        return Err(ScenarioCatalogError::new(
            "scenario names must contain 1 to 64 lowercase ASCII letters, digits, or hyphens",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        parse_catalog, FramePlan, ScenarioController, ScenarioControllerError, ScenarioKind,
        ScenarioLifecycle, ScenarioTarget, ACTIVATION_TOKEN_TTL, MAX_SCENARIO_CATALOG_ENTRIES,
        MAX_SCENARIO_ACTOR_LABEL_BYTES, MAX_SCENARIO_TARGETS, SCENARIO_CATALOG_VERSION,
    };

    const BASELINE_CATALOG: &str = include_str!("../scenarios/baseline.yaml");

    const OFFSET_CATALOG: &str = r#"
version: 1
scenarios:
  - name: offset-degraded
    kind: degraded_time
    start_frame_offset: 2
    lifecycle: transient
    duration_frames: 3
  - name: sustained-degraded
    kind: degraded_time
    start_frame_offset: 0
    lifecycle: sustained
"#;

    fn controller() -> ScenarioController {
        ScenarioController::new(parse_catalog(BASELINE_CATALOG).expect("baseline catalog compiles"))
    }

    fn endpoint(stream_id: u16) -> ScenarioTarget {
        ScenarioTarget::Endpoint { stream_id }
    }

    fn pdc(stream_id: u16, connection_id: u64) -> ScenarioTarget {
        ScenarioTarget::Pdc {
            stream_id,
            connection_id,
        }
    }

    #[test]
    fn compiles_the_shipped_baseline_catalog() {
        let catalog = parse_catalog(BASELINE_CATALOG).expect("baseline catalog must compile");

        assert_eq!(catalog.version, SCENARIO_CATALOG_VERSION);
        assert_eq!(catalog.scenarios().len(), 6);
        assert_eq!(
            catalog.scenario("degraded-time").expect("scenario exists").kind,
            ScenarioKind::DegradedTime
        );
        let excursion = catalog
            .scenario("signal-excursion")
            .expect("scenario exists");
        assert_eq!(excursion.lifecycle, ScenarioLifecycle::Transient);
        assert!(excursion.signal.is_some());
    }

    #[test]
    fn catalog_content_identity_changes_for_valid_content() {
        let baseline = parse_catalog(BASELINE_CATALOG).expect("baseline catalog compiles");
        let changed = parse_catalog(&BASELINE_CATALOG.replacen(
            "start_frame_offset: 0",
            "start_frame_offset: 1",
            1,
        ))
        .expect("changed catalog compiles");

        let baseline_identity = baseline.content_sha256();
        assert_eq!(baseline_identity.len(), 64);
        assert!(baseline_identity.bytes().all(|byte| {
            byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
        }));
        assert_ne!(baseline_identity, changed.content_sha256());
    }

    #[test]
    fn bounds_actor_labels_at_controller_ingress_without_changing_empty_labels() {
        let now = Instant::now();
        let mut controller = controller();

        let error = controller
            .prepare_activation(
                endpoint(1001),
                "degraded-time",
                Some("x".repeat(MAX_SCENARIO_ACTOR_LABEL_BYTES + 1)),
                now,
            )
            .expect_err("oversized actor labels must be rejected");
        assert!(matches!(
            error,
            ScenarioControllerError::InvalidActorLabel {
                maximum: MAX_SCENARIO_ACTOR_LABEL_BYTES
            }
        ));
        assert!(controller.prepared_actions().is_empty());

        let prepared = controller
            .prepare_activation(endpoint(1001), "degraded-time", Some(String::new()), now)
            .expect("empty actor labels retain their existing behavior");
        assert_eq!(prepared.actor_label.as_deref(), Some(""));
    }

    #[test]
    fn rejects_unknown_catalog_fields() {
        let contents = BASELINE_CATALOG.replace("version: 1", "version: 1\nunknown: true");

        let error = parse_catalog(&contents).expect_err("unknown field must fail");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_duplicate_scenario_names() {
        let contents = BASELINE_CATALOG.replace("name: recovery", "name: normal");

        let error = parse_catalog(&contents).expect_err("duplicate names must fail");

        assert!(error.to_string().contains("duplicate scenario names"));
    }

    #[test]
    fn rejects_catalogs_that_exceed_the_management_response_bound() {
        let mut contents = String::from("version: 1\nscenarios:\n");
        for index in 0..=MAX_SCENARIO_CATALOG_ENTRIES {
            contents.push_str(&format!(
                "  - name: scenario-{index}\n    kind: normal\n    start_frame_offset: 0\n    lifecycle: sustained\n"
            ));
        }

        let error = parse_catalog(&contents).expect_err("oversized catalog must fail at compile time");

        assert!(error.to_string().contains("management response limit"));
    }

    #[test]
    fn requires_a_positive_transient_duration() {
        let contents = BASELINE_CATALOG.replacen("duration_frames: 1", "duration_frames: 0", 1);

        let error = parse_catalog(&contents).expect_err("zero duration must fail");

        assert!(error.to_string().contains("positive duration_frames"));
    }

    #[test]
    fn requires_signal_values_for_signal_excursions() {
        let contents = BASELINE_CATALOG.replace(
            "    signal:\n      voltage_magnitude_delta: 2300.0\n      frequency_deviation_hz: 0.5\n      rocof_hz_per_s: 1.0\n",
            "",
        );

        let error = parse_catalog(&contents).expect_err("signal values must be required");

        assert!(error.to_string().contains("must define signal values"));
    }

    #[test]
    fn confirmed_activation_applies_at_the_next_boundary() {
        let now = Instant::now();
        let mut controller = controller();
        let prepared = controller
            .prepare_activation(endpoint(1001), "degraded-time", Some("operator".into()), now)
            .expect("activation prepares");

        assert_eq!(controller.frame_plan(1001), FramePlan::default());
        controller
            .confirm(prepared.token, now)
            .expect("activation confirms");
        assert!(controller.active_scenarios().is_empty());
        assert_eq!(controller.pending_actions().len(), 1);
        assert_eq!(controller.frame_plan(1001), FramePlan::default());

        controller
            .advance_boundary(40)
            .expect("first boundary advances");

        assert!(controller.frame_plan(1001).degraded_time);
        assert_eq!(controller.active_scenarios()[0].actor_label.as_deref(), Some("operator"));
    }

    #[test]
    fn transient_activation_respects_offset_and_expires_after_exact_duration() {
        let now = Instant::now();
        let catalog = parse_catalog(OFFSET_CATALOG).expect("offset catalog compiles");
        let mut controller = ScenarioController::new(catalog);
        let prepared = controller
            .prepare_activation(endpoint(1001), "offset-degraded", None, now)
            .expect("activation prepares");
        controller
            .confirm(prepared.token, now)
            .expect("activation confirms");

        controller.advance_boundary(10).expect("boundary 10 advances");
        assert!(controller.frame_plan(1001).is_noop());
        controller.advance_boundary(11).expect("boundary 11 advances");
        assert!(controller.frame_plan(1001).is_noop());
        controller.advance_boundary(12).expect("boundary 12 advances");
        assert!(controller.frame_plan(1001).degraded_time);
        controller.advance_boundary(14).expect("boundary 14 advances");
        assert!(controller.frame_plan(1001).degraded_time);
        controller.advance_boundary(15).expect("boundary 15 advances");
        assert!(controller.frame_plan(1001).is_noop());
        assert!(controller.active_scenarios().is_empty());
    }

    #[test]
    fn prepare_rejects_invalid_targets_and_kind_target_combinations() {
        let now = Instant::now();
        let mut controller = controller();

        let zero_stream = controller
            .prepare_activation(endpoint(0), "degraded-time", None, now)
            .expect_err("zero stream ID must fail");
        assert!(matches!(zero_stream, ScenarioControllerError::InvalidTarget { .. }));

        let endpoint_disconnect = controller
            .prepare_activation(endpoint(1001), "disconnect-pdc", None, now)
            .expect_err("disconnect must require a PDC target");
        assert!(matches!(
            endpoint_disconnect,
            ScenarioControllerError::IncompatibleTarget { .. }
        ));

        let pdc_degraded = controller
            .prepare_activation(pdc(1001, 0), "degraded-time", None, now)
            .expect_err("baseline scenarios must require endpoint targets");
        assert!(matches!(
            pdc_degraded,
            ScenarioControllerError::IncompatibleTarget { .. }
        ));
    }

    #[test]
    fn conflicting_actions_for_the_same_exact_target_are_rejected() {
        let now = Instant::now();
        let mut controller = controller();
        let first = controller
            .prepare_activation(endpoint(1001), "degraded-time", None, now)
            .expect("first preparation succeeds");

        let pending_conflict = controller
            .prepare_activation(endpoint(1001), "missing-frames", None, now)
            .expect_err("second preparation must conflict");
        assert!(matches!(
            pending_conflict,
            ScenarioControllerError::TargetBusy { .. }
        ));

        controller.confirm(first.token, now).expect("first confirms");
        let confirmed_conflict = controller
            .prepare_activation(endpoint(1001), "missing-frames", None, now)
            .expect_err("confirmed action must retain the target reservation");
        assert!(matches!(
            confirmed_conflict,
            ScenarioControllerError::TargetBusy { .. }
        ));
    }

    #[test]
    fn rejects_scenario_targets_beyond_the_bounded_state_capacity() {
        let now = Instant::now();
        let mut controller = controller();

        for stream_id in 1..=150 {
            controller
                .prepare_activation(endpoint(stream_id), "normal", None, now)
                .expect("endpoint target must fit the state capacity");
            for connection_id in 1..=2 {
                controller
                    .prepare_activation(
                        pdc(stream_id, connection_id),
                        "disconnect-pdc",
                        None,
                        now,
                    )
                    .expect("PDC target must fit the state capacity");
            }
        }

        let error = controller
            .prepare_activation(endpoint(151), "normal", None, now)
            .expect_err("additional target must exceed the state capacity");

        assert!(matches!(
            error,
            ScenarioControllerError::TargetCapacityExceeded {
                maximum: MAX_SCENARIO_TARGETS
            }
        ));
    }

    #[test]
    fn expired_tokens_are_rejected_consumed_and_release_the_target() {
        let now = Instant::now();
        let mut controller = controller();
        let prepared = controller
            .prepare_activation(endpoint(1001), "degraded-time", None, now)
            .expect("activation prepares");

        let expired = controller
            .confirm(prepared.token, now + ACTIVATION_TOKEN_TTL)
            .expect_err("token expires at its deadline");
        assert!(matches!(expired, ScenarioControllerError::ExpiredToken { .. }));

        let reused = controller
            .confirm(prepared.token, now + ACTIVATION_TOKEN_TTL)
            .expect_err("expired token cannot be reused");
        assert!(matches!(reused, ScenarioControllerError::UnknownToken { .. }));

        controller
            .prepare_activation(
                endpoint(1001),
                "missing-frames",
                None,
                now + ACTIVATION_TOKEN_TTL,
            )
            .expect("expired preparation releases its target");
    }

    #[test]
    fn cancel_consumes_live_preparations_releases_targets_and_is_one_shot() {
        let now = Instant::now();
        let mut controller = controller();
        let prepared = controller
            .prepare_activation(
                endpoint(1001),
                "degraded-time",
                Some("preparer".to_owned()),
                now,
            )
            .expect("activation prepares");
        assert_eq!(prepared.confirm_expires_in_ms, 60_000);
        assert_eq!(
            serde_json::to_value(&prepared)
                .expect("prepared snapshot serializes")["token"]
                .as_str(),
            Some("1")
        );

        let cancelled = controller
            .cancel(prepared.token, now + Duration::from_millis(250))
            .expect("live preparation cancels");
        assert_eq!(cancelled.actor_label.as_deref(), Some("preparer"));
        assert_eq!(cancelled.confirm_expires_in_ms, 59_750);
        assert!(controller.prepared_actions().is_empty());
        assert!(controller.pending_actions().is_empty());
        assert!(controller.active_scenarios().is_empty());

        let retry = controller
            .cancel(prepared.token, now + Duration::from_millis(250))
            .expect_err("cancellation is one-shot");
        assert!(matches!(retry, ScenarioControllerError::UnknownToken { .. }));
        controller
            .prepare_activation(endpoint(1001), "missing-frames", None, now)
            .expect("cancellation releases its target");
    }

    #[test]
    fn cancelling_an_expired_preparation_consumes_it_and_releases_its_target() {
        let now = Instant::now();
        let mut controller = controller();
        let prepared = controller
            .prepare_activation(endpoint(1001), "degraded-time", None, now)
            .expect("activation prepares");

        let expired = controller
            .cancel(prepared.token, now + ACTIVATION_TOKEN_TTL)
            .expect_err("expired preparation cannot be cancelled");
        assert!(matches!(expired, ScenarioControllerError::ExpiredToken { .. }));
        let retry = controller
            .cancel(prepared.token, now + ACTIVATION_TOKEN_TTL)
            .expect_err("expired cancellation is one-shot");
        assert!(matches!(retry, ScenarioControllerError::UnknownToken { .. }));
        controller
            .prepare_activation(endpoint(1001), "missing-frames", None, now + ACTIVATION_TOKEN_TTL)
            .expect("expired cancellation releases its target");
    }

    #[test]
    fn confirmation_and_cancellation_are_resolved_by_the_first_token_consumer() {
        let now = Instant::now();
        let mut confirmed_first = controller();
        let confirmed = confirmed_first
            .prepare_activation(endpoint(1001), "degraded-time", None, now)
            .expect("activation prepares");
        confirmed_first
            .confirm(confirmed.token, now)
            .expect("confirmation consumes the token");
        let cancelled_after_confirmation = confirmed_first
            .cancel(confirmed.token, now)
            .expect_err("confirmed token cannot be cancelled");
        assert!(matches!(
            cancelled_after_confirmation,
            ScenarioControllerError::UnknownToken { .. }
        ));
        assert_eq!(confirmed_first.pending_actions().len(), 1);

        let mut cancelled_first = controller();
        let cancelled = cancelled_first
            .prepare_activation(endpoint(1001), "degraded-time", None, now)
            .expect("activation prepares");
        cancelled_first
            .cancel(cancelled.token, now)
            .expect("cancellation consumes the token");
        let confirmed_after_cancellation = cancelled_first
            .confirm(cancelled.token, now)
            .expect_err("cancelled token cannot be confirmed");
        assert!(matches!(
            confirmed_after_cancellation,
            ScenarioControllerError::UnknownToken { .. }
        ));
        assert!(cancelled_first.pending_actions().is_empty());
    }

    #[test]
    fn snapshots_exclude_expired_preparations() {
        let now = Instant::now();
        let mut controller = controller();
        controller
            .prepare_activation(endpoint(1001), "degraded-time", None, now)
            .expect("activation prepares");

        let snapshot = controller.snapshot(now + ACTIVATION_TOKEN_TTL);

        assert!(snapshot.prepared.is_empty());
    }

    #[test]
    fn confirmed_clear_removes_a_sustained_scenario_at_the_next_boundary() {
        let now = Instant::now();
        let catalog = parse_catalog(OFFSET_CATALOG).expect("clear catalog compiles");
        let mut controller = ScenarioController::new(catalog);
        let activation = controller
            .prepare_activation(endpoint(1001), "sustained-degraded", None, now)
            .expect("activation prepares");
        controller
            .confirm(activation.token, now)
            .expect("activation confirms");
        controller.advance_boundary(10).expect("activation boundary advances");
        assert!(controller.frame_plan(1001).degraded_time);

        let clear = controller
            .prepare_clear(endpoint(1001), Some("operator".into()), now)
            .expect("clear prepares");
        controller.confirm(clear.token, now).expect("clear confirms");
        assert!(controller.frame_plan(1001).degraded_time);

        controller.advance_boundary(11).expect("clear boundary advances");
        assert!(controller.frame_plan(1001).is_noop());
        assert!(controller.active_scenarios().is_empty());
    }

    #[test]
    fn frame_plan_composes_endpoint_effects_and_scoped_pdc_disconnects() {
        let now = Instant::now();
        let mut controller = controller();
        let requests = [
            (endpoint(1001), "degraded-time"),
            (endpoint(1002), "missing-frames"),
            (endpoint(1003), "signal-excursion"),
            (pdc(1001, 9), "disconnect-pdc"),
            (pdc(1001, 7), "disconnect-pdc"),
            (pdc(1002, 7), "disconnect-pdc"),
        ];

        for (target, scenario_name) in requests {
            let prepared = controller
                .prepare_activation(target, scenario_name, None, now)
                .expect("scenario prepares");
            controller.confirm(prepared.token, now).expect("scenario confirms");
        }
        controller.advance_boundary(50).expect("boundary advances");

        let first = controller.frame_plan(1001);
        assert!(first.degraded_time);
        assert!(!first.omit_data);
        assert!(first.signal.is_none());
        assert_eq!(first.disconnect_pdc_connection_ids, vec![7, 9]);

        let second = controller.frame_plan(1002);
        assert!(!second.degraded_time);
        assert!(second.omit_data);
        assert_eq!(second.disconnect_pdc_connection_ids, vec![7]);

        let third = controller.frame_plan(1003);
        assert!(third.signal.is_some());
        assert!(third.disconnect_pdc_connection_ids.is_empty());
        assert!(controller.frame_plan(1004).is_noop());
    }
}