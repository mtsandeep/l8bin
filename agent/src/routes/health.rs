use axum::{extract::State, Json};
use litebin_common::types::HealthReport;
use sysinfo::System;

use crate::AgentState;

/// GET /health
/// Returns system resource info and running container count.
pub async fn health(State(state): State<AgentState>) -> Json<HealthReport> {
    let mut sys = System::new_all();
    sys.refresh_all();

    let memory_total = sys.total_memory();
    let memory_available = sys.available_memory();
    let cpu_cores = sys.cpus().len() as u32;

    let (disk_free, disk_total) = litebin_common::sys::disk_space();

    // Container count from Docker
    let container_count = state
        .docker
        .running_container_count()
        .await
        .unwrap_or(0);

    let image_stats = state.docker.image_stats().await;

    Json(HealthReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        memory_available,
        memory_total,
        cpu_cores,
        disk_free,
        disk_total,
        container_count,
        image_stats,
        public_ip: if state.config.public_ip.is_empty() {
            None
        } else {
            Some(state.config.public_ip.clone())
        },
    })
}
