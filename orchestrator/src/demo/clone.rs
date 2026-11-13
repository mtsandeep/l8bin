//! Demo instance creation — a copy-engine clone of the demo parent on the
//! parent's own node, with fresh or baseline-seeded volumes.
//!
//! Everything rides the duplicate machinery minus what a same-node demo clone
//! never needs: no image transfer (same daemon), no route copying (the demo
//! host belongs to the parent), no source stop (the parent keeps running —
//! baseline seeds are frozen copies). Callers: ensure_instances, the
//! gatekeeper's claim path, and the janitor's prewarm refill.

use crate::copy_engine;
use crate::demo::{self, baseline::Baseline};
use crate::status::{self, ProjectUpdateFields};
use crate::AppState;
use litebin_common::types::{Project, ProjectStatus};

/// Options for spawning a demo instance.
pub struct CloneOpts {
    /// Start the instance after creation (prewarms yes, pool spawns yes).
    pub start: bool,
    /// Seed volumes + project dir from the captured baseline (empty otherwise).
    pub baseline: Option<Baseline>,
    /// Requested id (edit/shared instances); pool instances auto-name.
    pub requested_id: Option<String>,
}

/// Create a demo instance of `parent`. Returns the new project id immediately;
/// payload work runs in the background (status starts at `importing`).
pub async fn create_instance(state: &AppState, parent: &Project, opts: CloneOpts) -> anyhow::Result<String> {
    let reserved = [state.platform.dashboard_subdomain(), state.platform.poke_subdomain()];
    let base = format!("{}-demo", parent.id);
    let alloc =
        copy_engine::allocate_project_id(&state.db, &reserved, opts.requested_id.as_deref(), &base).await?;

    insert_instance_rows(state, parent, &alloc.id).await?;

    let new_id = alloc.id.clone();
    let parent_id = parent.id.clone();
    let state_bg = state.clone();
    let lock = clone_lock(&parent_id).await;
    tokio::spawn(async move {
        let _guard = lock.lock().await;
        let job = CloneJob { parent_id, new_id: new_id.clone(), start: opts.start, baseline: opts.baseline };
        if let Err(e) = run_clone(&state_bg, &job).await {
            tracing::error!(project = %new_id, parent = %job.parent_id, error = %e, "demo clone failed");
            crate::routes::deploy::logs::push_deploy_log(&state_bg, &new_id, &format!("demo clone failed: {e}"));
            // Clean the partial instance immediately — the visitor-facing error
            // page only appears in session mode; either way nothing lingers.
            demo::teardown_instance(&state_bg, &new_id).await;
        }
    });
    Ok(alloc.id)
}

/// One clone payload per parent at a time — concurrent local tar streams
/// into the same daemon can deadlock.
static CLONE_LOCKS: std::sync::LazyLock<dashmap::DashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>> =
    std::sync::LazyLock::new(|| dashmap::DashMap::new());

async fn clone_lock(parent_id: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    CLONE_LOCKS.entry(parent_id.to_string()).or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(()))).clone()
}

struct CloneJob {
    parent_id: String,
    new_id: String,
    start: bool,
    baseline: Option<Baseline>,
}

async fn run_clone(state: &AppState, job: &CloneJob) -> anyhow::Result<()> {
    let (parent_id, new_id) = (&job.parent_id, &job.new_id);
    let parent: Project = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(parent_id)
        .fetch_one(&state.db)
        .await?;
    let node = demo::parent_node(&parent);
    let logs = crate::routes::deploy::logs::push_deploy_log;

    tracing::info!(project = %new_id, parent = %parent_id, "demo clone: starting");
    logs(state, new_id, &format!("demo instance cloning from '{parent_id}'"));

    // Compose projects need their project dir (compose.yaml, .env, binds);
    // single-service image projects start from the DB spec alone.
    if parent.deploy_type == Some(litebin_common::types::DeployType::Compose) {
        match &job.baseline {
            Some(b) if b.project_tar.is_some() || !b.volumes.is_empty() => {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(180),
                    demo::baseline::restore(state, parent_id, new_id, b),
                )
                .await
                {
                    Ok(Ok(())) => {
                        tracing::info!(project = %new_id, "demo clone: baseline restored");
                        logs(state, new_id, "baseline restored");
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(_) => anyhow::bail!("baseline restore timed out after 180s"),
                }
            }
            _ => {
                let tmp = copy_engine::spool_dir(state);
                copy_engine::transfer_project_dir(state, &node, &node, parent_id, new_id, true, &tmp).await?;
                logs(state, new_id, "project files copied");
            }
        }
    }

    status::transition(&state.db, new_id, ProjectStatus::Stopped, &ProjectUpdateFields::default(), None).await?;

    if job.start {
        logs(state, new_id, "starting");
        match start_with_name_conflict_retry(state, new_id).await {
            Ok(_) => logs(state, new_id, "started"),
            Err((code, message)) => anyhow::bail!("start failed ({code}): {message}"),
        }
    }

    let _ = state.route_sync_tx.send(());
    logs(state, new_id, "demo instance ready");
    Ok(())
}

/// The id allocator only checks the DB; a container from a torn-down instance
/// can still hold the name. On a 409, clear stale containers and retry once.
async fn start_with_name_conflict_retry(
    state: &AppState,
    new_id: &str,
) -> Result<(), (axum::http::StatusCode, String)> {
    match crate::routes::manage::handlers::start_project(
        axum::extract::State(state.clone()),
        axum::extract::Path(new_id.to_string()),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err((_code, message)) if message.contains("already in use") => {
            tracing::warn!(project = %new_id, "demo clone: name held by a stale container, clearing and retrying");
            let _ = state.docker.remove_by_name(new_id).await;
            crate::routes::manage::handlers::start_project(
                axum::extract::State(state.clone()),
                axum::extract::Path(new_id.to_string()),
            )
            .await
            .map(|_| ())
        }
        Err(e) => Err(e),
    }
}

/// DB rows for the instance: spec copied from the parent, volumes re-scoped,
/// routes NOT copied (the demo host belongs to the parent). Modeled on
/// duplicate's `insert_copy_rows`.
async fn insert_instance_rows(state: &AppState, parent: &Project, new_id: &str) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let node_id_for_db = parent.node_id.clone().filter(|n| n != "local");

    sqlx::query(
        "INSERT INTO projects (id, user_id, name, description, is_background, image, internal_port, \
         status, node_id, cmd, memory_limit_mb, cpu_limit, auto_stop_enabled, auto_stop_timeout_mins, \
         auto_start_enabled, allow_raw_ports, allow_docker_access, volumes, service_count, service_summary, \
         deploy_type, demo_parent_id, created_at, updated_at) \
         VALUES (?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id)
    .bind(&parent.user_id)
    .bind(&parent.description)
    .bind(parent.is_background)
    .bind(&parent.image)
    .bind(parent.internal_port)
    .bind(ProjectStatus::Importing)
    .bind(node_id_for_db)
    .bind(&parent.cmd)
    .bind(parent.memory_limit_mb)
    .bind(parent.cpu_limit)
    .bind(parent.auto_stop_enabled)
    .bind(parent.auto_stop_timeout_mins)
    .bind(parent.auto_start_enabled)
    .bind(parent.allow_raw_ports)
    .bind(parent.allow_docker_access)
    .bind(&parent.volumes)
    .bind(parent.service_count)
    .bind(parent.service_summary.clone())
    .bind(parent.deploy_type.clone())
    .bind(&parent.id)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;

    sqlx::query(
        "INSERT OR IGNORE INTO project_capabilities (project_id, capability, granted_at) \
         SELECT ?, capability, granted_at FROM project_capabilities WHERE project_id = ?",
    )
    .bind(new_id)
    .bind(&parent.id)
    .execute(&state.db)
    .await?;

    sqlx::query(
        "INSERT INTO project_services (project_id, service_name, image, port, cmd, is_public, depends_on, \
         memory_limit_mb, cpu_limit, status, is_oneshot) \
         SELECT ?, service_name, image, port, cmd, is_public, depends_on, memory_limit_mb, cpu_limit, 'stopped', is_oneshot \
         FROM project_services WHERE project_id = ?",
    )
    .bind(new_id)
    .bind(&parent.id)
    .execute(&state.db)
    .await?;

    let volumes: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT volume_name, container_path, service_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
    )
    .bind(&parent.id)
    .fetch_all(&state.db)
    .await?;
    for (scoped_name, container_path, service_name) in volumes {
        sqlx::query("INSERT INTO project_volumes (project_id, service_name, volume_name, container_path) VALUES (?, ?, ?, ?)")
            .bind(new_id)
            .bind(service_name)
            .bind(copy_engine::rescope_volume(&scoped_name, &parent.id, new_id))
            .bind(container_path)
            .execute(&state.db)
            .await?;
    }
    Ok(())
}
