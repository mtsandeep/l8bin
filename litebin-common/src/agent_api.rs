//! Typed wire contract for orchestrator ↔ agent HTTP calls.
//!
//! Every internal agent endpoint's request/response body is defined here once and
//! used by BOTH sides: the orchestrator serializes requests / deserializes
//! responses through these types via `orchestrator::nodes::client::AgentClient`,
//! and the agent's axum handlers extract/respond with them directly. Renaming a
//! field therefore breaks compilation on the other side instead of failing at
//! runtime in production.
//!
//! Compatibility rules for this module:
//! - Field names and serde attributes are the wire format; do not change them
//!   without a `protocol_version` bump (see `types::HealthReport`).
//! - Requests: `Option` fields serialize as `null` when `None`. The agent's
//!   deserialization treats absent and `null` identically, so either shape is
//!   valid on the wire.
//! - Responses: fields with `skip_serializing_if` also carry `serde(default)`
//!   so clients tolerate their absence.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::VolumeMount;

// ── Endpoint paths ────────────────────────────────────────────────────────────

pub const RUN_PATH: &str = "/containers/run";
pub const RECREATE_PATH: &str = "/containers/recreate";
pub const START_PATH: &str = "/containers/start";
pub const STOP_PATH: &str = "/containers/stop";
pub const STOP_SERVICE_PATH: &str = "/containers/stop-service";
pub const STOP_PROJECT_PATH: &str = "/containers/stop-project";
pub const REMOVE_PATH: &str = "/containers/remove";
pub const CLEANUP_PATH: &str = "/containers/cleanup";
pub const BATCH_RUN_PATH: &str = "/containers/batch-run";
pub const CONTAINER_STATS_PATH: &str = "/containers/stats";
pub const CONTAINER_SCAN_PATH: &str = "/containers/scan";
pub const CONTAINER_IMPORT_PATH: &str = "/containers/import";
pub const CONTAINER_COMPOSE_FILE_PATH: &str = "/containers/compose-file";
pub const IMAGES_LOAD_PATH: &str = "/images/load";
pub const IMAGES_INSPECT_PATH: &str = "/images/inspect";
pub const IMAGES_REMOVE_UNUSED_PATH: &str = "/images/remove-unused";
pub const IMAGES_PRUNE_PATH: &str = "/images/prune";
pub const CADDY_SYNC_PATH: &str = "/caddy/sync";
pub const PROJECT_META_PATH: &str = "/internal/project-meta";

// ── Shared helpers ────────────────────────────────────────────────────────────

pub fn default_false() -> bool {
    false
}

/// Uniform error body returned by agent endpoints on failure.
#[derive(Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

// ── Single-container lifecycle (/containers/run|recreate|start|…) ─────────────

#[derive(Serialize, Deserialize)]
pub struct RunRequest {
    pub image: String,
    pub internal_port: Option<i64>,
    pub project_id: String,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub volumes: Option<Vec<VolumeMount>>,
    #[serde(default = "default_false")]
    pub docker_observe: bool,
    /// When true, only create the project directory, `.env` placeholder, and metadata.
    /// No image pull or container start.
    #[serde(default = "default_false")]
    pub stage_only: bool,
}

#[derive(Serialize, Deserialize)]
pub struct RunResponse {
    pub container_id: String,
    pub mapped_port: Option<u16>,
}

#[derive(Serialize, Deserialize)]
pub struct StartRequest {
    pub container_id: String,
    pub project_id: Option<String>,
    pub image: Option<String>,
    pub internal_port: Option<i64>,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    #[serde(default = "default_false")]
    pub host_network: bool,
    #[serde(default = "default_false")]
    pub is_background: bool,
}

#[derive(Serialize, Deserialize)]
pub struct StartResponse {
    pub mapped_port: u16,
}

#[derive(Serialize, Deserialize)]
pub struct StopRequest {
    pub container_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct StopServiceRequest {
    pub project_id: String,
    pub service_name: String,
}

#[derive(Serialize, Deserialize)]
pub struct StopServiceResponse {
    pub stopped: bool,
}

#[derive(Serialize, Deserialize)]
pub struct StopProjectRequest {
    pub project_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct StopProjectResponse {
    pub stopped_containers: usize,
}

#[derive(Serialize, Deserialize)]
pub struct RemoveRequest {
    pub container_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct CleanupRequest {
    pub project_id: String,
    pub volumes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct LogsQuery {
    pub tail: Option<usize>,
}

// ── Batch run (/containers/batch-run) ─────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct BatchRunRequest {
    pub project_id: String,
    pub compose_yaml: String,
    /// Ordered list of service names to start (topologically sorted by orchestrator).
    pub service_order: Vec<String>,
    /// If Some, only recreate these services (partial redeploy). If None, deploy all.
    pub target_services: Option<Vec<String>>,
    pub allow_raw_ports: Option<bool>,
    pub docker_observe: Option<bool>,
    pub host_network: Option<bool>,
    #[serde(default = "default_false")]
    pub is_background: bool,
    /// Whether to force-pull images (true) or skip if already present locally (false).
    #[serde(default = "default_false")]
    pub force_pull: bool,
    /// When true, only persist compose.yaml and create the runtime `.env` placeholder.
    /// No networks, pulls, or containers are started.
    #[serde(default = "default_false")]
    pub stage_only: bool,
    /// Per-service resource overrides from dashboard (service_name → {memory_limit_mb, cpu_limit}).
    /// Applied on top of compose-embedded limits; None values mean "use global default".
    pub service_resources: Option<HashMap<String, ServiceResources>>,
    /// Global default memory limit (MB) from orchestrator settings. Used when neither
    /// the compose YAML nor per-service overrides specify a memory limit.
    pub default_memory_limit_mb: Option<i64>,
    /// Global default CPU limit from orchestrator settings. Used when neither
    /// the compose YAML nor per-service overrides specify a CPU limit.
    pub default_cpu_limit: Option<f64>,
}

/// Per-service resource overrides sent by the orchestrator.
#[derive(Serialize, Deserialize)]
pub struct ServiceResources {
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
}

#[derive(Serialize, Deserialize)]
pub struct BatchRunResponse {
    pub services: Vec<ServiceRunResult>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ServiceRunResult {
    pub service_name: String,
    pub container_id: Option<String>,
    pub mapped_port: Option<u16>,
    pub error: Option<String>,
}

/// Error body for a failed batch run; `affected_services` lists services whose
/// previous containers were removed before the failure (not restorable by the agent).
#[derive(Serialize, Deserialize)]
pub struct BatchRunErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub affected_services: Vec<String>,
}

// ── Stats (/containers/stats, per-container GETs) ─────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct BatchStatsRequest {
    pub container_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ContainerStatsResponse {
    pub container_id: String,
    pub state: String,
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
    pub disk_gb: f64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_limit: Option<f64>,
}

// ── Scan & import (/containers/scan|import|compose-file) ──────────────────────

#[derive(Serialize, Deserialize)]
pub struct ContainerImportSpec {
    pub container_id: String,
    pub new_name: String,
}

#[derive(Serialize, Deserialize)]
pub struct ImportRequest {
    pub project_id: String,
    pub network_name: String,
    pub containers: Vec<ContainerImportSpec>,
    pub compose_yaml: Option<String>,
    pub env_content: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ContainerImportResult {
    pub container_id: String,
    pub new_name: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ImportResponse {
    pub results: Vec<ContainerImportResult>,
    pub errors: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ComposeFileResponse {
    pub compose_yaml: Option<String>,
    pub env_content: Option<String>,
}

// ── Images (/images/*) ────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct LoadImageResponse {
    pub image_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct RemoveImageRequest {
    pub image: String,
}

#[derive(Serialize, Deserialize)]
pub struct RemoveImageResponse {
    pub removed: bool,
}

#[derive(Serialize, Deserialize)]
pub struct PruneResponse {
    pub bytes_reclaimed: u64,
}

#[derive(Serialize, Deserialize)]
pub struct LoadImageQueryParams {
    pub image_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct InspectQueryParams {
    pub image: String,
}

#[derive(Serialize, Deserialize)]
pub struct InspectResponse {
    pub image_id: String,
}

// ── Node registration & project meta (/internal/*) ───────────────────────────

#[derive(Serialize, Deserialize)]
pub struct RegisterRequest {
    pub node_id: String,
    pub secret: String,
    pub domain: String,
    pub wake_report_url: String,
    pub heartbeat_url: String,
}

#[derive(Serialize, Deserialize)]
pub struct ProjectMetaRequest {
    pub projects: HashMap<String, bool>,
    pub background_projects: Option<HashMap<String, bool>>,
    pub allow_raw_ports: Option<HashMap<String, bool>>,
    pub docker_observe: Option<HashMap<String, bool>>,
    pub host_network: Option<HashMap<String, bool>>,
    /// Global default memory limit (MB) from orchestrator settings.
    pub default_memory_limit_mb: Option<i64>,
    /// Global default CPU limit from orchestrator settings.
    pub default_cpu_limit: Option<f64>,
}

// ── Wire-format tests ─────────────────────────────────────────────────────────
// Pin the serialized field names so edits to these structs cannot silently
// drift the protocol between deployed orchestrators and agents.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_run_request_serializes_expected_fields() {
        let req = BatchRunRequest {
            project_id: "myapp".into(),
            compose_yaml: "services: {}".into(),
            service_order: vec!["web".into()],
            target_services: None,
            allow_raw_ports: Some(false),
            docker_observe: Some(true),
            host_network: None,
            is_background: false,
            force_pull: true,
            stage_only: false,
            service_resources: None,
            default_memory_limit_mb: Some(256),
            default_cpu_limit: None,
        };

        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["project_id"], "myapp");
        assert_eq!(v["compose_yaml"], "services: {}");
        assert_eq!(v["service_order"][0], "web");
        assert_eq!(v["target_services"], serde_json::Value::Null);
        assert_eq!(v["allow_raw_ports"], false);
        assert_eq!(v["docker_observe"], true);
        assert_eq!(v["host_network"], serde_json::Value::Null);
        assert_eq!(v["is_background"], false);
        assert_eq!(v["force_pull"], true);
        assert_eq!(v["stage_only"], false);
        assert_eq!(v["default_memory_limit_mb"], 256);
        assert_eq!(v["default_cpu_limit"], serde_json::Value::Null);
    }

    #[test]
    fn batch_run_request_tolerates_absent_optional_fields() {
        // The stage call site omits capability/resource fields entirely.
        let v: BatchRunRequest =
            serde_json::from_str(r#"{"project_id":"p","compose_yaml":"y","service_order":[]}"#).unwrap();
        assert!(!v.is_background);
        assert!(!v.force_pull);
        assert!(!v.stage_only);
        assert!(v.service_resources.is_none());
    }

    #[test]
    fn batch_run_response_round_trip_skips_empty_warnings() {
        let resp = BatchRunResponse { services: vec![], warnings: vec![] };
        let v = serde_json::to_value(&resp).unwrap();
        assert!(v.get("warnings").is_none(), "empty warnings must be omitted");

        let back: BatchRunResponse = serde_json::from_value(v).unwrap();
        assert!(back.warnings.is_empty());
    }

    #[test]
    fn service_run_result_round_trip() {
        let r = ServiceRunResult {
            service_name: "web".into(),
            container_id: Some("abc".into()),
            mapped_port: Some(3000),
            error: None,
        };
        let back: ServiceRunResult = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(back.service_name, "web");
        assert_eq!(back.mapped_port, Some(3000));
    }

    #[test]
    fn run_request_serializes_expected_fields() {
        let req = RunRequest {
            image: "nginx".into(),
            internal_port: Some(80),
            project_id: "p".into(),
            cmd: None,
            memory_limit_mb: Some(128),
            cpu_limit: None,
            volumes: None,
            docker_observe: false,
            stage_only: true,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["image"], "nginx");
        assert_eq!(v["internal_port"], 80);
        assert_eq!(v["project_id"], "p");
        assert_eq!(v["memory_limit_mb"], 128);
        assert_eq!(v["docker_observe"], false);
        assert_eq!(v["stage_only"], true);
    }

    #[test]
    fn run_response_tolerates_absent_mapped_port() {
        // stage_only runs return container_id with no mapped_port.
        let r: RunResponse = serde_json::from_str(r#"{"container_id":"x"}"#).unwrap();
        assert_eq!(r.container_id, "x");
        assert_eq!(r.mapped_port, None);
    }

    #[test]
    fn start_request_round_trip() {
        let req = StartRequest {
            container_id: "c".into(),
            project_id: Some("p".into()),
            image: None,
            internal_port: None,
            cmd: None,
            memory_limit_mb: None,
            cpu_limit: None,
            host_network: false,
            is_background: true,
        };
        let back: StartRequest = serde_json::from_value(serde_json::to_value(&req).unwrap()).unwrap();
        assert_eq!(back.container_id, "c");
        assert_eq!(back.project_id.as_deref(), Some("p"));
        assert!(back.is_background);
        assert!(!back.host_network);
    }

    #[test]
    fn register_request_requires_all_fields() {
        // heartbeat_url is required — the drift bug this contract exists to prevent.
        assert!(
            serde_json::from_str::<RegisterRequest>(
                r#"{"node_id":"n","secret":"s","domain":"d","wake_report_url":"w"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<RegisterRequest>(
                r#"{"node_id":"n","secret":"s","domain":"d","wake_report_url":"w","heartbeat_url":"h"}"#
            )
            .is_ok()
        );
    }

    #[test]
    fn error_and_batch_error_bodies_round_trip() {
        let e: ErrorResponse = serde_json::from_str(r#"{"error":"boom"}"#).unwrap();
        assert_eq!(e.error, "boom");

        let v: BatchRunErrorResponse =
            serde_json::from_str(r#"{"error":"failed","affected_services":["db"]}"#).unwrap();
        assert_eq!(v.affected_services, vec!["db".to_string()]);

        let omitted =
            serde_json::to_value(BatchRunErrorResponse { error: "failed".into(), affected_services: vec![] }).unwrap();
        assert!(omitted.get("affected_services").is_none());
    }

    #[test]
    fn container_stats_response_tolerates_absent_cpu_limit() {
        let s: ContainerStatsResponse =
            serde_json::from_str(r#"{"container_id":"c","state":"running","cpu_percent":1.0,"memory_usage":1,"memory_limit":2,"disk_gb":0.5}"#)
                .unwrap();
        assert!(s.cpu_limit.is_none());
    }

    #[test]
    fn compose_file_response_round_trip() {
        let r: ComposeFileResponse =
            serde_json::from_str(r#"{"compose_yaml":"services: {}","env_content":null}"#).unwrap();
        assert_eq!(r.compose_yaml.as_deref(), Some("services: {}"));
        assert!(r.env_content.is_none());
    }
}
