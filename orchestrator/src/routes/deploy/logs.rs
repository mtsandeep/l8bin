use crate::AppState;

/// Push a timestamped log line into the in-memory deploy log buffer for a project.
pub fn push_deploy_log(state: &AppState, project_id: &str, message: &str) {
    let entry = state.deploy_logs
        .entry(project_id.to_string())
        .or_insert_with(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut logs) = entry.lock() {
        let ts = chrono::Utc::now().format("%H:%M:%S").to_string();
        logs.push(format!("[{}] {}", ts, message));
    }
}

/// Remove all deploy logs for a project (called after deploy completes).
pub fn clear_deploy_logs(state: &AppState, project_id: &str) {
    state.deploy_logs.remove(project_id);
}
