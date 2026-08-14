use super::reconcile::sync_all_local_from_docker;

/// Run periodic status sync every 60 seconds.
/// Corrects any drift between DB status and actual Docker container state.
pub async fn run_periodic_sync(state: crate::AppState, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let interval = std::time::Duration::from_secs(60);
    tracing::info!("periodic status sync started (interval: 60s)");

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                tracing::info!("periodic status sync shutting down");
                break;
            }
            _ = tokio::time::sleep(interval) => {
                let changed = sync_all_local_from_docker(&state.db, &state.docker).await;
                if !changed.is_empty() {
                    tracing::info!(count = changed.len(), "periodic sync: corrected project statuses");
                    let _ = state.route_sync_tx.send(());
                }
            }
        }
    }
}
