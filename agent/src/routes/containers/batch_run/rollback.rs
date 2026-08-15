use axum::{Json, http::StatusCode, response::IntoResponse};

use super::types::BatchRunErrorResponse;

#[cfg(test)]
pub(super) static FAIL_NEXT_PROXY_READINESS_CHECK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(super) async fn wait_for_proxy_ready(
    docker: &litebin_common::docker::DockerManager,
    container_id: &str,
) -> anyhow::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_PROXY_READINESS_CHECK.swap(false, std::sync::atomic::Ordering::SeqCst) {
        anyhow::bail!("test-injected proxy readiness failure");
    }
    docker.wait_for_healthy(container_id, true).await
}

pub(super) fn batch_run_error(
    status: StatusCode,
    error: impl Into<String>,
    affected_services: &[String],
) -> axum::response::Response {
    let mut affected_services = affected_services.to_vec();
    affected_services.sort();
    affected_services.dedup();
    (status, Json(BatchRunErrorResponse { error: error.into(), affected_services })).into_response()
}

pub(super) async fn rollback_started_containers(
    docker: &litebin_common::docker::DockerManager,
    project_id: &str,
    service_names: &[String],
    container_ids: &[String],
) {
    for container_id in container_ids.iter().rev() {
        let _ = docker.stop_container(container_id).await;
        let _ = docker.remove_container(container_id).await;
    }
    for service_name in service_names {
        let _ = docker.remove_by_service_name(project_id, service_name, None).await;
    }
}
