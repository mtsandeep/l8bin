use axum::{extract::State, Json};
use litebin_common::types::HealthReport;
use sysinfo::{Disks, System};

use crate::AgentState;

/// GET /health
/// Returns system resource info and running container count.
pub async fn health(State(state): State<AgentState>) -> Json<HealthReport> {
    let mut sys = System::new_all();
    sys.refresh_all();

    let memory_total = sys.total_memory();
    let memory_available = sys.available_memory();
    let cpu_cores = sys.cpus().len() as u32;

    // Disk free — sum free space across all disks
    let disks = Disks::new_with_refreshed_list();
    let disk_free: u64 = disks.iter().map(|d| d.available_space()).sum();
    let disk_total: u64 = disks.iter().map(|d| d.total_space()).sum();

    // Container count from Docker
    let container_count = state
        .docker
        .running_container_count()
        .await
        .unwrap_or(0);

    let image_stats = state.docker.image_stats().await;

    Json(HealthReport {
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
