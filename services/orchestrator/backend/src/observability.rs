//! Low-overhead production observability for the embedded HTTP server.
//!
//! Metrics and JSON request logs are always available. OTLP/HTTP trace export is
//! opt-in and uses a bounded, non-blocking queue so an unavailable collector can
//! never delay a control-plane request.

#![allow(clippy::missing_const_for_thread_local)]

use crate::durable::DurableStore;
use crate::http::{ApiRequest, ApiResponse};
use anyhow::{Context, Result, anyhow};
use orchestrator_control_plane::DurableOperation;
use orchestrator_storage::{ControlPlaneAnomalyCounters, JobMetricsSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LATENCY_BUCKETS_MS: [u64; 9] = [5, 10, 25, 50, 100, 200, 500, 1_000, 5_000];
const DEFAULT_OTLP_QUEUE: usize = 1_024;
const MAX_CORRELATION_VALUES: usize = 8;
const CONTROL_PLANE_ANOMALY_NAMESPACE: &str = "observability";
const CONTROL_PLANE_ANOMALY_STATE_KEY: &str = "control-plane-anomalies-v1";
const CONTROL_PLANE_ANOMALY_SCHEMA_VERSION: u16 = 1;
const MAX_ANOMALY_WINDOW_IDENTITIES: usize = 4_096;

#[derive(Debug, Default)]
struct MetricsState {
    requests: BTreeMap<(String, String, u16), u64>,
    latency_buckets: BTreeMap<(String, String), [u64; LATENCY_BUCKETS_MS.len()]>,
    latency_sum_ms: BTreeMap<(String, String), u128>,
    latency_count: BTreeMap<(String, String), u64>,
}

#[derive(Debug, Default)]
struct Metrics {
    active_requests: AtomicU64,
    overload_rejections: AtomicU64,
    otlp_dropped_spans: AtomicU64,
    state: Mutex<MetricsState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ControlPlaneAnomalyState {
    schema_version: u16,
    #[serde(default)]
    control_plane_process_starts_total: u64,
    #[serde(default, skip_serializing)]
    expired_job_lease_transitions_total: u64,
    #[serde(default, skip_serializing)]
    operation_over_300_seconds_transitions_total: u64,
    #[serde(default)]
    operation_invalid_updated_at_transitions_total: u64,
    /// v8 compatibility only. v9 migrates matching identities into the
    /// transactional lease marker table, then omits this field on rewrite.
    #[serde(default, skip_serializing)]
    active_expired_leases: BTreeMap<String, String>,
    /// v8 compatibility only. v9 seeds matching active DB markers and clears
    /// this field after upgrading the durable counter floor.
    #[serde(default, skip_serializing)]
    active_over_300_operation_episodes: BTreeSet<String>,
    #[serde(default = "default_terminal_operation_cursor_ms", skip_serializing)]
    terminal_operation_cursor_ms: i64,
    /// Legacy finish-time cursor ties; v9 write-path counters no longer use it.
    #[serde(default, skip_serializing)]
    terminal_operation_cursor_episodes: BTreeSet<String>,
    #[serde(default)]
    active_invalid_operation_timestamps: BTreeSet<String>,
    /// One-candidate compatibility bridge for state written by the first RC
    /// implementation. Initialization moves this bounded set into the active
    /// window and immediately rewrites the record without the legacy field.
    #[serde(default, skip_serializing)]
    observed_over_300_operation_episodes: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_operation_collection_error: Option<String>,
}

impl Default for ControlPlaneAnomalyState {
    fn default() -> Self {
        Self {
            schema_version: CONTROL_PLANE_ANOMALY_SCHEMA_VERSION,
            control_plane_process_starts_total: 0,
            expired_job_lease_transitions_total: 0,
            operation_over_300_seconds_transitions_total: 0,
            operation_invalid_updated_at_transitions_total: 0,
            active_expired_leases: BTreeMap::new(),
            active_over_300_operation_episodes: BTreeSet::new(),
            terminal_operation_cursor_ms: -1,
            terminal_operation_cursor_episodes: BTreeSet::new(),
            active_invalid_operation_timestamps: BTreeSet::new(),
            observed_over_300_operation_episodes: BTreeSet::new(),
            active_operation_collection_error: None,
        }
    }
}

const fn default_terminal_operation_cursor_ms() -> i64 {
    -1
}

#[derive(Debug, Default)]
struct ControlPlaneAnomalyMetrics {
    /// Serializes the complete read -> classify -> durable checkpoint cycle.
    /// A state-only lock is insufficient because an older clean DB snapshot
    /// could otherwise commit after a newer anomalous snapshot.
    observation: Mutex<()>,
    /// `None` is a visible, fail-closed initialization boundary. Production
    /// initializes this from durable state before any lease recovery runs.
    state: Mutex<Option<ControlPlaneAnomalyState>>,
    /// SQLite/PostgreSQL is the sole truth for lease and long-Operation
    /// counters. These counters are advanced in the same transaction as the
    /// corresponding state transition and only cached here for rendering.
    durable_counters: Mutex<Option<ControlPlaneAnomalyCounters>>,
    observation_errors: AtomicU64,
}

#[derive(Debug, Clone)]
struct OtlpExporter {
    sender: SyncSender<Value>,
}

/// Per-server observability state. Keeping this state out of process globals
/// makes embedded Desktop instances and tests independent from one another.
#[derive(Debug)]
pub(crate) struct Observability {
    service_name: String,
    metrics: Metrics,
    process_started_at_ms: u64,
    control_plane_anomalies: ControlPlaneAnomalyMetrics,
    otlp: Option<OtlpExporter>,
}

impl Observability {
    pub(crate) fn from_env() -> Result<Arc<Self>> {
        let otlp = OtlpExporter::from_env()?;
        if otlp.is_some() {
            emit_json(json!({
                "timestamp_ms": unix_time_ms(),
                "level": "INFO",
                "event": "otel_exporter_enabled",
                "service": "ojos-orchestrator",
            }));
        }
        Ok(Arc::new(Self {
            service_name: "ojos-orchestrator".to_string(),
            metrics: Metrics::default(),
            process_started_at_ms: unix_time_ms(),
            control_plane_anomalies: ControlPlaneAnomalyMetrics::default(),
            otlp,
        }))
    }

    /// Loads the cumulative anomaly counters before recovery can mutate Job
    /// state. The generic state record is shared by SQLite and PostgreSQL, so
    /// counter continuity survives a daemon restart and a controlled failover
    /// of the single-active control plane.
    pub(crate) fn initialize_control_plane_anomalies(
        &self,
        store: Option<&DurableStore>,
    ) -> Result<()> {
        let _observation = self
            .control_plane_anomalies
            .observation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = match store {
            Some(store) => store
                .get_state::<ControlPlaneAnomalyState>(
                    CONTROL_PLANE_ANOMALY_NAMESPACE,
                    CONTROL_PLANE_ANOMALY_STATE_KEY,
                )
                .context("load durable control-plane anomaly counters")?
                .unwrap_or_default(),
            None => ControlPlaneAnomalyState::default(),
        };
        if state.schema_version != CONTROL_PLANE_ANOMALY_SCHEMA_VERSION {
            return Err(anyhow!(
                "unsupported control-plane anomaly state schema version {}",
                state.schema_version
            ));
        }
        if state.observed_over_300_operation_episodes.len() > MAX_ANOMALY_WINDOW_IDENTITIES {
            return Err(anyhow!(
                "legacy control-plane anomaly window contains {} identities; maximum is {}",
                state.observed_over_300_operation_episodes.len(),
                MAX_ANOMALY_WINDOW_IDENTITIES
            ));
        }
        state
            .active_over_300_operation_episodes
            .append(&mut state.observed_over_300_operation_episodes);
        validate_anomaly_window_capacity(&state)?;
        if u64::try_from(state.active_expired_leases.len()).unwrap_or(u64::MAX)
            > state.expired_job_lease_transitions_total
            || u64::try_from(state.active_over_300_operation_episodes.len()).unwrap_or(u64::MAX)
                > state.operation_over_300_seconds_transitions_total
        {
            return Err(anyhow!(
                "legacy control-plane anomaly active episodes exceed their cumulative counters"
            ));
        }
        let durable_counters = match store {
            Some(store) => store
                .operation_store()
                .migrate_legacy_anomaly_state(
                    state.expired_job_lease_transitions_total,
                    state.operation_over_300_seconds_transitions_total,
                    &state.active_expired_leases,
                    &state.active_over_300_operation_episodes,
                )
                .map_err(|error| anyhow!("migrate v9 control-plane anomaly evidence: {error}"))?,
            None => ControlPlaneAnomalyCounters {
                expired_job_lease_transitions_total: state.expired_job_lease_transitions_total,
                operation_over_300_seconds_transitions_total: state
                    .operation_over_300_seconds_transitions_total,
            },
        };
        // v9 tables are now the sole truth for these counters and active
        // episodes. Clear the compatibility fields before rewriting generic
        // state so a later restart cannot seed or count them a second time.
        state.expired_job_lease_transitions_total = 0;
        state.operation_over_300_seconds_transitions_total = 0;
        state.active_expired_leases.clear();
        state.active_over_300_operation_episodes.clear();
        state.terminal_operation_cursor_ms = default_terminal_operation_cursor_ms();
        state.terminal_operation_cursor_episodes.clear();
        state.observed_over_300_operation_episodes.clear();
        state.control_plane_process_starts_total =
            state.control_plane_process_starts_total.saturating_add(1);
        if let Some(store) = store {
            store
                .put_state(
                    CONTROL_PLANE_ANOMALY_NAMESPACE,
                    CONTROL_PLANE_ANOMALY_STATE_KEY,
                    &state,
                )
                .context("checkpoint durable control-plane process generation")?;
        }
        *self
            .control_plane_anomalies
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(state);
        *self
            .control_plane_anomalies
            .durable_counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(durable_counters);
        Ok(())
    }

    /// Test seam for the bounded active-Operation evidence path. The storage
    /// transaction rechecks every supplied candidate before inserting a
    /// marker and incrementing the durable counter.

    fn observe_control_plane_snapshot_locked(
        &self,
        store: &DurableStore,
        operations: &[DurableOperation],
        now_ms: i64,
    ) -> Result<()> {
        if let Err(error) =
            ensure_identity_capacity("active Operation anomaly candidates", operations.len())
        {
            self.record_control_plane_observation_error();
            return Err(error);
        }
        let mut valid_candidates = Vec::with_capacity(operations.len());
        let mut invalid_timestamps = BTreeSet::new();
        for operation in operations {
            let episode = operation_episode_identity(operation);
            if invalid_operation_timestamps(operation, now_ms) {
                invalid_timestamps.insert(episode);
                continue;
            }
            valid_candidates.push(operation.clone());
        }
        let durable_counters = match store
            .operation_store()
            .observe_active_operation_anomalies(&valid_candidates, now_ms)
        {
            Ok(counters) => counters,
            Err(error) => {
                self.record_control_plane_observation_error();
                return Err(anyhow!("observe durable Operation anomalies: {error}"));
            }
        };
        self.update_control_plane_anomalies(Some(store), |state| {
            ensure_identity_capacity(
                "invalid Operation timestamp window",
                invalid_timestamps.len(),
            )?;
            let invalid_delta = invalid_timestamps
                .difference(&state.active_invalid_operation_timestamps)
                .count();
            let invalid_delta = u64::try_from(invalid_delta)
                .map_err(|_| anyhow!("invalid Operation timestamp delta exceeds u64"))?;

            let changed = invalid_delta > 0
                || state.active_invalid_operation_timestamps != invalid_timestamps
                || state.active_operation_collection_error.is_some();
            state.operation_invalid_updated_at_transitions_total = state
                .operation_invalid_updated_at_transitions_total
                .saturating_add(invalid_delta);
            state.active_invalid_operation_timestamps = invalid_timestamps;
            state.active_operation_collection_error = None;
            validate_anomaly_window_capacity(state)?;
            Ok(changed)
        })?;
        *self
            .control_plane_anomalies
            .durable_counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(durable_counters);
        Ok(())
    }

    /// Reads only the constant-size Job counter projection plus currently
    /// active Operations for `/metrics`; retained terminal histories are never
    /// materialized. Terminal long-Operation evidence is captured atomically
    /// by the Operation CAS write path.
    pub(crate) fn observe_durable_control_plane(
        &self,
        store: &DurableStore,
        now_ms: i64,
    ) -> Result<JobMetricsSnapshot> {
        let _observation = self
            .control_plane_anomalies
            .observation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let job_metrics = match store.job_store().metrics_snapshot(now_ms) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.record_control_plane_observation_error();
                return Err(anyhow!("read bounded durable Job metrics: {error}"));
            }
        };
        let operations = match store.operation_store().anomaly_candidates() {
            Ok(operations) => operations,
            Err(error) => {
                self.observe_operation_collection_error_locked(store, &error.to_string())?;
                return Err(anyhow!(
                    "list bounded active Operations for anomaly observation: {error}"
                ));
            }
        };
        self.observe_control_plane_snapshot_locked(store, &operations, now_ms)?;
        Ok(job_metrics)
    }

    /// A malformed durable Operation (including a missing `updated_at_ms`)
    /// makes the typed list undecodable. Count the error episode as an invalid
    /// timestamp anomaly and return failure to the metrics caller.

    fn observe_operation_collection_error_locked(
        &self,
        store: &DurableStore,
        detail: &str,
    ) -> Result<()> {
        let fingerprint = sha256_hex(detail.as_bytes());
        self.control_plane_anomalies
            .observation_errors
            .fetch_add(1, Ordering::Relaxed);
        self.update_control_plane_anomalies(Some(store), |state| {
            if state.active_operation_collection_error.as_deref() == Some(&fingerprint) {
                return Ok(false);
            }
            state.operation_invalid_updated_at_transitions_total = state
                .operation_invalid_updated_at_transitions_total
                .saturating_add(1);
            state.active_operation_collection_error = Some(fingerprint);
            Ok(true)
        })
    }

    pub(crate) fn record_control_plane_observation_error(&self) {
        self.control_plane_anomalies
            .observation_errors
            .fetch_add(1, Ordering::Relaxed);
    }

    fn update_control_plane_anomalies(
        &self,
        store: Option<&DurableStore>,
        update: impl FnOnce(&mut ControlPlaneAnomalyState) -> Result<bool>,
    ) -> Result<()> {
        let mut guard = self
            .control_plane_anomalies
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = guard
            .as_ref()
            .ok_or_else(|| anyhow!("control-plane anomaly counters are not initialized"))?;
        let mut next = current.clone();
        let changed = match update(&mut next) {
            Ok(changed) => changed,
            Err(error) => {
                self.control_plane_anomalies
                    .observation_errors
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error.context("classify durable control-plane anomalies"));
            }
        };
        if !changed {
            return Ok(());
        }
        if let Some(store) = store
            && let Err(error) = store.put_state(
                CONTROL_PLANE_ANOMALY_NAMESPACE,
                CONTROL_PLANE_ANOMALY_STATE_KEY,
                &next,
            )
        {
            self.control_plane_anomalies
                .observation_errors
                .fetch_add(1, Ordering::Relaxed);
            return Err(anyhow!("persist control-plane anomaly counters: {error}"));
        }
        *guard = Some(next);
        Ok(())
    }

    pub(crate) fn render_prometheus(&self) -> String {
        let mut output = String::from(
            "# HELP ojos_orchestrator_http_requests_total Completed HTTP requests.\n\
             # TYPE ojos_orchestrator_http_requests_total counter\n",
        );
        let state = self
            .metrics
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for ((method, route, status), count) in &state.requests {
            output.push_str(&format!(
                "ojos_orchestrator_http_requests_total{{method=\"{}\",route=\"{}\",status=\"{}\"}} {}\n",
                prometheus_escape(method),
                prometheus_escape(route),
                status,
                count
            ));
        }
        output.push_str(
            "# HELP ojos_orchestrator_http_request_duration_milliseconds HTTP request latency.\n\
             # TYPE ojos_orchestrator_http_request_duration_milliseconds histogram\n",
        );
        for ((method, route), buckets) in &state.latency_buckets {
            let labels = format!(
                "method=\"{}\",route=\"{}\"",
                prometheus_escape(method),
                prometheus_escape(route)
            );
            for (index, upper) in LATENCY_BUCKETS_MS.iter().enumerate() {
                output.push_str(&format!(
                    "ojos_orchestrator_http_request_duration_milliseconds_bucket{{{labels},le=\"{upper}\"}} {}\n",
                    buckets[index]
                ));
            }
            let count = state
                .latency_count
                .get(&(method.clone(), route.clone()))
                .copied()
                .unwrap_or_default();
            let sum = state
                .latency_sum_ms
                .get(&(method.clone(), route.clone()))
                .copied()
                .unwrap_or_default();
            output.push_str(&format!(
                "ojos_orchestrator_http_request_duration_milliseconds_bucket{{{labels},le=\"+Inf\"}} {count}\n"
            ));
            output.push_str(&format!(
                "ojos_orchestrator_http_request_duration_milliseconds_sum{{{labels}}} {sum}\n"
            ));
            output.push_str(&format!(
                "ojos_orchestrator_http_request_duration_milliseconds_count{{{labels}}} {count}\n"
            ));
        }
        drop(state);
        output.push_str(
            "# HELP ojos_orchestrator_http_active_requests Requests currently executing.\n\
             # TYPE ojos_orchestrator_http_active_requests gauge\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_http_active_requests {}\n",
            self.metrics.active_requests.load(Ordering::Relaxed)
        ));
        output.push_str(
            "# HELP ojos_orchestrator_overload_rejections_total Connections rejected by admission control.\n\
             # TYPE ojos_orchestrator_overload_rejections_total counter\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_overload_rejections_total {}\n",
            self.metrics.overload_rejections.load(Ordering::Relaxed)
        ));
        output.push_str(
            "# HELP ojos_orchestrator_otel_dropped_spans_total Spans dropped before non-blocking OTLP export.\n\
             # TYPE ojos_orchestrator_otel_dropped_spans_total counter\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_otel_dropped_spans_total {}\n",
            self.metrics.otlp_dropped_spans.load(Ordering::Relaxed)
        ));
        let anomalies = self
            .control_plane_anomalies
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let durable_counters = self
            .control_plane_anomalies
            .durable_counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .to_owned();
        let state_loaded = u8::from(anomalies.is_some() && durable_counters.is_some());
        let anomalies = anomalies.unwrap_or_default();
        let durable_counters = durable_counters.unwrap_or_default();
        output.push_str(
            "# HELP ojos_orchestrator_expired_job_lease_transitions_total Durable Job leases first observed past their deadline.\n\
             # TYPE ojos_orchestrator_expired_job_lease_transitions_total counter\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_expired_job_lease_transitions_total {}\n",
            durable_counters.expired_job_lease_transitions_total
        ));
        output.push_str(
            "# HELP ojos_orchestrator_operation_over_300_seconds_transitions_total Operations first observed or durably reconstructed as active for more than 300 seconds.\n\
             # TYPE ojos_orchestrator_operation_over_300_seconds_transitions_total counter\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_operation_over_300_seconds_transitions_total {}\n",
            durable_counters.operation_over_300_seconds_transitions_total
        ));
        output.push_str(
            "# HELP ojos_orchestrator_operation_invalid_updated_at_transitions_total Operations first observed with missing, non-positive, future, inconsistent, or undecodable lifecycle timestamp evidence.\n\
             # TYPE ojos_orchestrator_operation_invalid_updated_at_transitions_total counter\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_operation_invalid_updated_at_transitions_total {}\n",
            anomalies.operation_invalid_updated_at_transitions_total
        ));
        output.push_str(
            "# HELP ojos_orchestrator_control_plane_anomaly_observation_errors_total Failed durable anomaly observations or state checkpoints.\n\
             # TYPE ojos_orchestrator_control_plane_anomaly_observation_errors_total counter\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_control_plane_anomaly_observation_errors_total {}\n",
            self.control_plane_anomalies
                .observation_errors
                .load(Ordering::Relaxed)
        ));
        output.push_str(
            "# HELP ojos_orchestrator_control_plane_anomaly_state_loaded Whether durable anomaly state was initialized before serving.\n\
             # TYPE ojos_orchestrator_control_plane_anomaly_state_loaded gauge\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_control_plane_anomaly_state_loaded {state_loaded}\n"
        ));
        output.push_str(
            "# HELP ojos_orchestrator_control_plane_process_starts_total Durable count of control-plane process initializations for this database.\n\
             # TYPE ojos_orchestrator_control_plane_process_starts_total counter\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_control_plane_process_starts_total {}\n",
            anomalies.control_plane_process_starts_total
        ));
        output.push_str(
            "# HELP ojos_orchestrator_process_start_time_seconds Unix time when this daemon process initialized observability.\n\
             # TYPE ojos_orchestrator_process_start_time_seconds gauge\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_process_start_time_seconds {}.{:03}\n",
            self.process_started_at_ms / 1_000,
            self.process_started_at_ms % 1_000,
        ));
        let process = process_snapshot();
        output.push_str(
            "# HELP ojos_orchestrator_process_resident_memory_bytes Resident memory used by the daemon process.\n\
             # TYPE ojos_orchestrator_process_resident_memory_bytes gauge\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_process_resident_memory_bytes {}\n",
            process.resident_memory_bytes
        ));
        output.push_str(
            "# HELP ojos_orchestrator_process_threads Operating-system threads owned by the daemon process.\n\
             # TYPE ojos_orchestrator_process_threads gauge\n",
        );
        output.push_str(&format!(
            "ojos_orchestrator_process_threads {}\n",
            process.threads
        ));
        output
    }

    pub(crate) fn record_overload(&self) {
        self.metrics
            .overload_rejections
            .fetch_add(1, Ordering::Relaxed);
        emit_json(json!({
            "timestamp_ms": unix_time_ms(),
            "level": "WARN",
            "event": "http_overload_rejected",
            "service": self.service_name,
            "status": 503,
            "retry_after_seconds": 1,
        }));
    }

    fn finish_request(&self, request: RequestObservation) {
        self.metrics.active_requests.fetch_sub(1, Ordering::Relaxed);
        let duration_ms = request
            .started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let status = request.status.unwrap_or(500);
        let route = normalized_route(&request.path);
        {
            let mut state = self
                .metrics
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *state
                .requests
                .entry((request.method.clone(), route.clone(), status))
                .or_default() += 1;
            let buckets = state
                .latency_buckets
                .entry((request.method.clone(), route.clone()))
                .or_insert([0; LATENCY_BUCKETS_MS.len()]);
            for (index, upper) in LATENCY_BUCKETS_MS.iter().enumerate() {
                if duration_ms <= *upper {
                    buckets[index] += 1;
                }
            }
            *state
                .latency_sum_ms
                .entry((request.method.clone(), route.clone()))
                .or_default() += u128::from(duration_ms);
            *state
                .latency_count
                .entry((request.method.clone(), route.clone()))
                .or_default() += 1;
        }

        let level = if status >= 500 {
            "ERROR"
        } else if status >= 400 {
            "WARN"
        } else {
            "INFO"
        };
        emit_json(json!({
            "timestamp_ms": unix_time_ms(),
            "level": level,
            "event": "http_request_completed",
            "service": self.service_name,
            "request_id": request.request_id,
            "traceparent": request.traceparent,
            "method": request.method,
            "path": request.path,
            "route": route,
            "peer": request.peer,
            "status": status,
            "duration_ms": duration_ms,
            "operation_ids": request.operation_ids,
            "job_ids": request.job_ids,
            "node_ids": request.node_ids,
            "resource_statuses": request.resource_statuses,
        }));

        if let Some(exporter) = &self.otlp {
            let span = request.as_otlp_span(&self.service_name, &route, status, duration_ms);
            if matches!(exporter.sender.try_send(span), Err(TrySendError::Full(_))) {
                self.metrics
                    .otlp_dropped_spans
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn ensure_identity_capacity(window: &str, identities: usize) -> Result<()> {
    if identities > MAX_ANOMALY_WINDOW_IDENTITIES {
        return Err(anyhow!(
            "{window} contains {identities} identities; maximum is {MAX_ANOMALY_WINDOW_IDENTITIES}"
        ));
    }
    Ok(())
}

fn validate_anomaly_window_capacity(state: &ControlPlaneAnomalyState) -> Result<()> {
    ensure_identity_capacity("expired lease window", state.active_expired_leases.len())?;
    ensure_identity_capacity(
        "active over-300-second Operation window",
        state.active_over_300_operation_episodes.len(),
    )?;
    ensure_identity_capacity(
        "terminal Operation cursor tie window",
        state.terminal_operation_cursor_episodes.len(),
    )?;
    ensure_identity_capacity(
        "invalid Operation timestamp window",
        state.active_invalid_operation_timestamps.len(),
    )?;
    Ok(())
}

fn operation_episode_identity(operation: &DurableOperation) -> String {
    format!(
        "{}:{}:{}",
        operation.operation_id,
        operation.generation,
        operation
            .started_at_ms
            .map_or_else(|| "not-started".to_string(), |value| value.to_string())
    )
}

fn invalid_operation_timestamps(operation: &DurableOperation, now_ms: i64) -> bool {
    if operation.updated_at_ms <= 0 || operation.updated_at_ms > now_ms {
        return true;
    }
    if operation
        .started_at_ms
        .is_some_and(|started_at_ms| started_at_ms <= 0 || started_at_ms > now_ms)
    {
        return true;
    }
    if !operation.status.is_terminal() {
        return operation.finished_at_ms.is_some();
    }
    let Some(finished_at_ms) = operation.finished_at_ms else {
        return true;
    };
    finished_at_ms <= 0
        || finished_at_ms > now_ms
        || finished_at_ms > operation.updated_at_ms
        || operation
            .started_at_ms
            .is_some_and(|started_at_ms| finished_at_ms < started_at_ms)
}

fn sha256_hex(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl OtlpExporter {
    fn from_env() -> Result<Option<Self>> {
        let Some(mut endpoint) = std::env::var("ORCHESTRATOR_OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let parsed = endpoint
            .parse::<ureq::http::Uri>()
            .map_err(|error| anyhow!("invalid OTLP endpoint: {error}"))?;
        let scheme = parsed.scheme_str().unwrap_or_default();
        let host = parsed.host().unwrap_or_default();
        let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]");
        if scheme != "https" && !(scheme == "http" && loopback) {
            return Err(anyhow!(
                "ORCHESTRATOR_OTEL_EXPORTER_OTLP_ENDPOINT must use HTTPS (HTTP is allowed only for loopback)"
            ));
        }
        if !endpoint.ends_with("/v1/traces") {
            endpoint = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
        }
        let timeout = std::env::var("ORCHESTRATOR_OTEL_EXPORT_TIMEOUT_MS")
            .ok()
            .map(|value| value.parse::<u64>())
            .transpose()
            .context("ORCHESTRATOR_OTEL_EXPORT_TIMEOUT_MS must be an integer")?
            .unwrap_or(2_000);
        if !(100..=30_000).contains(&timeout) {
            return Err(anyhow!(
                "ORCHESTRATOR_OTEL_EXPORT_TIMEOUT_MS must be between 100 and 30000"
            ));
        }
        let queue = std::env::var("ORCHESTRATOR_OTEL_QUEUE_CAPACITY")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("ORCHESTRATOR_OTEL_QUEUE_CAPACITY must be an integer")?
            .unwrap_or(DEFAULT_OTLP_QUEUE);
        if !(1..=65_536).contains(&queue) {
            return Err(anyhow!(
                "ORCHESTRATOR_OTEL_QUEUE_CAPACITY must be between 1 and 65536"
            ));
        }
        let (sender, receiver) = mpsc::sync_channel::<Value>(queue);
        thread::Builder::new()
            .name("orchestrator-otlp-export".to_string())
            .spawn(move || {
                let agent: ureq::Agent = ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_millis(timeout)))
                    .http_status_as_error(false)
                    .max_redirects(0)
                    .build()
                    .into();
                while let Ok(span) = receiver.recv() {
                    let body = json!({
                        "resourceSpans": [{
                            "resource": {"attributes": [{
                                "key": "service.name",
                                "value": {"stringValue": "ojos-orchestrator"}
                            }]},
                            "scopeSpans": [{
                                "scope": {"name": "ojos.orchestrator.http", "version": "1.0.0"},
                                "spans": [span]
                            }]
                        }]
                    });
                    let result = agent
                        .post(&endpoint)
                        .header("Content-Type", "application/json")
                        .send(serde_json::to_vec(&body).unwrap_or_default());
                    if let Err(error) = result {
                        emit_json(json!({
                            "timestamp_ms": unix_time_ms(),
                            "level": "WARN",
                            "event": "otel_export_failed",
                            "service": "ojos-orchestrator",
                            "error": error.to_string(),
                        }));
                    }
                }
            })
            .context("spawn OTLP exporter")?;
        Ok(Some(Self { sender }))
    }
}

#[derive(Debug)]
struct RequestObservation {
    method: String,
    path: String,
    peer: String,
    traceparent: Option<String>,
    started: Instant,
    started_unix_ns: u128,
    status: Option<u16>,
    request_id: Option<String>,
    operation_ids: BTreeSet<String>,
    job_ids: BTreeSet<String>,
    node_ids: BTreeSet<String>,
    resource_statuses: BTreeSet<String>,
}

impl RequestObservation {
    fn as_otlp_span(
        &self,
        service_name: &str,
        route: &str,
        status: u16,
        duration_ms: u64,
    ) -> Value {
        let mut trace_id = [0_u8; 16];
        let mut span_id = [0_u8; 8];
        let _ = getrandom::fill(&mut trace_id);
        let _ = getrandom::fill(&mut span_id);
        let end = self.started_unix_ns + u128::from(duration_ms) * 1_000_000;
        json!({
            "traceId": hex(&trace_id),
            "spanId": hex(&span_id),
            "name": format!("{} {}", self.method, route),
            "kind": 2,
            "startTimeUnixNano": self.started_unix_ns.to_string(),
            "endTimeUnixNano": end.to_string(),
            "attributes": [
                otlp_attribute("service.name", service_name),
                otlp_attribute("http.request.method", &self.method),
                otlp_attribute("http.route", route),
                otlp_attribute("http.response.status_code", &status.to_string()),
            ],
            "status": {"code": if status >= 500 { 2 } else { 1 }},
        })
    }
}

thread_local! {
    static CURRENT_REQUEST: RefCell<Option<(Arc<Observability>, RequestObservation)>> = const { RefCell::new(None) };
}

pub(crate) struct RequestGuard;

impl Drop for RequestGuard {
    fn drop(&mut self) {
        CURRENT_REQUEST.with(|current| {
            if let Some((observability, request)) = current.borrow_mut().take() {
                observability.finish_request(request);
            }
        });
    }
}

pub(crate) fn begin_request(
    observability: Arc<Observability>,
    request: &ApiRequest,
    peer: String,
) -> RequestGuard {
    observability
        .metrics
        .active_requests
        .fetch_add(1, Ordering::Relaxed);
    let traceparent = request
        .headers
        .get("traceparent")
        .filter(|value| value.len() <= 128)
        .cloned();
    let observation = RequestObservation {
        method: request.method.clone(),
        path: request.path.split('?').next().unwrap_or("/").to_string(),
        peer,
        traceparent,
        started: Instant::now(),
        started_unix_ns: unix_time_ns(),
        status: None,
        request_id: None,
        operation_ids: BTreeSet::new(),
        job_ids: BTreeSet::new(),
        node_ids: BTreeSet::new(),
        resource_statuses: BTreeSet::new(),
    };
    CURRENT_REQUEST.with(|current| {
        *current.borrow_mut() = Some((observability, observation));
    });
    RequestGuard
}

/// Called centrally by the HTTP writer, before the response body is moved.
pub(crate) fn record_response(response: &ApiResponse) {
    CURRENT_REQUEST.with(|current| {
        let mut current = current.borrow_mut();
        let Some((_, request)) = current.as_mut() else {
            return;
        };
        request.status = Some(response.status);
        request.request_id = response
            .headers
            .get("X-Request-ID")
            .or_else(|| response.headers.get("x-request-id"))
            .cloned()
            .or_else(|| {
                response
                    .body
                    .get("request_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        collect_correlation_fields(
            &response.body,
            &mut request.operation_ids,
            &mut request.job_ids,
            &mut request.node_ids,
            &mut request.resource_statuses,
        );
    });
}

pub(crate) fn record_status(status: u16) {
    CURRENT_REQUEST.with(|current| {
        if let Some((_, request)) = current.borrow_mut().as_mut() {
            request.status = Some(status);
        }
    });
}

fn collect_correlation_fields(
    value: &Value,
    operations: &mut BTreeSet<String>,
    jobs: &mut BTreeSet<String>,
    nodes: &mut BTreeSet<String>,
    statuses: &mut BTreeSet<String>,
) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if let Some(text) = value.as_str() {
                    match key.as_str() {
                        "operation_id" | "rollback_of_operation_id" => {
                            insert_correlation(operations, text)
                        }
                        "job_id" => insert_correlation(jobs, text),
                        "node_id" | "target_node_id" => insert_correlation(nodes, text),
                        "status" | "desired_state" | "observed_state" => {
                            insert_correlation(statuses, text)
                        }
                        _ => {}
                    }
                }
                collect_correlation_fields(value, operations, jobs, nodes, statuses);
            }
        }
        Value::Array(values) => {
            for value in values.iter().take(128) {
                collect_correlation_fields(value, operations, jobs, nodes, statuses);
            }
        }
        _ => {}
    }
}

fn insert_correlation(target: &mut BTreeSet<String>, value: &str) {
    if target.len() < MAX_CORRELATION_VALUES {
        target.insert(value.chars().take(256).collect());
    }
}

fn normalized_route(path: &str) -> String {
    let segments = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return "/".to_string();
    }
    let mut route = String::new();
    for (index, segment) in segments.iter().enumerate() {
        route.push('/');
        let stable = matches!(
            *segment,
            "api"
                | "v1"
                | "healthz"
                | "live"
                | "ready"
                | "auth"
                | "oidc"
                | "start"
                | "callback"
                | "session"
                | "logout"
                | "desktop"
                | "exchange"
                | "catalog"
                | "sources"
                | "releases"
                | "store"
                | "nodes"
                | "deployments"
                | "topologies"
                | "revisions"
                | "status"
                | "operations"
                | "logs"
                | "events"
                | "diagnostics"
                | "export"
                | "agent"
                | "jobs"
                | "metrics"
        ) || segment.starts_with(':');
        if stable || index < 2 {
            route.push_str(segment);
        } else {
            route.push_str(":id");
        }
    }
    route
}

fn otlp_attribute(key: &str, value: &str) -> Value {
    json!({"key": key, "value": {"stringValue": value}})
}

fn prometheus_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn emit_json(value: Value) {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    let _ = serde_json::to_writer(&mut lock, &value);
    let _ = lock.write_all(b"\n");
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn unix_time_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[derive(Debug, Default)]
struct ProcessSnapshot {
    resident_memory_bytes: u64,
    threads: u64,
}

#[cfg(target_os = "linux")]
fn process_snapshot() -> ProcessSnapshot {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return ProcessSnapshot::default();
    };
    let mut snapshot = ProcessSnapshot::default();
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            snapshot.resident_memory_bytes = value
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default()
                .saturating_mul(1_024);
        } else if let Some(value) = line.strip_prefix("Threads:") {
            snapshot.threads = value.trim().parse().unwrap_or_default();
        }
    }
    snapshot
}

#[cfg(windows)]
fn process_snapshot() -> ProcessSnapshot {
    use std::ffi::c_void;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }
    let mut counters = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<ProcessMemoryCounters>() as u32,
        )
    } != 0;
    ProcessSnapshot {
        resident_memory_bytes: if ok {
            counters.working_set_size.min(u64::MAX as usize) as u64
        } else {
            0
        },
        // The Linux soak runner also enforces thread growth. Windows release
        // RSS remains available; zero declares thread enumeration unavailable.
        threads: 0,
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn process_snapshot() -> ProcessSnapshot {
    ProcessSnapshot::default()
}
