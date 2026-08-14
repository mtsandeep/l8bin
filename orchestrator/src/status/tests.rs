use sqlx::SqlitePool;

use litebin_common::types::ProjectStatus;

use super::*;

async fn status_db() -> SqlitePool {
    let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("src/db/migrations").run(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO users
         (id, username, password_hash, is_admin, created_at, updated_at)
         VALUES ('test-user', 'test-user', 'test-hash', 0, 0, 0)",
    )
    .execute(&db)
    .await
    .unwrap();
    db
}

async fn insert_project(db: &SqlitePool, project_id: &str, service_names: &[&str]) {
    sqlx::query(
        "INSERT INTO projects (id, user_id, status, created_at, updated_at, deploy_type, is_background, service_count)
         VALUES (?, 'test-user', 'deploying', 0, 0, 'compose', 1, ?)",
    )
    .bind(project_id)
    .bind(service_names.len() as i64)
    .execute(db)
    .await
    .unwrap();
    for service_name in service_names {
        sqlx::query(
            "INSERT INTO project_services
             (project_id, service_name, image, status, is_oneshot)
             VALUES (?, ?, 'registry.invalid/generic-fixture:latest', 'deploying', 0)",
        )
        .bind(project_id)
        .bind(service_name)
        .execute(db)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn one_service_compose_completion_is_identical_for_local_and_remote() {
    let db = status_db().await;
    for project_id in ["local-background", "remote-background"] {
        insert_project(&db, project_id, &["collector"]).await;
        set_service_running(&db, project_id, "collector", "container-id", None).await.unwrap();

        assert_eq!(derive_and_set_project_status(&db, project_id).await, ProjectStatus::Running);
        let stored: ProjectStatus = sqlx::query_scalar("SELECT status FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(stored, ProjectStatus::Running);
    }
}

#[tokio::test]
async fn partial_failure_clears_affected_runtime_and_preserves_unrelated_service() {
    let db = status_db().await;
    insert_project(&db, "partial-failure", &["collector", "unrelated"]).await;
    for service_name in ["collector", "unrelated"] {
        set_service_running(&db, "partial-failure", service_name, &format!("{service_name}-container"), None)
            .await
            .unwrap();
    }

    set_service_replacement_error(&db, "partial-failure", "collector").await.unwrap();
    set_project_error_only(&db, "partial-failure").await.unwrap();

    let affected: (ProjectStatus, Option<String>) = sqlx::query_as(
        "SELECT status, container_id FROM project_services
         WHERE project_id = 'partial-failure' AND service_name = 'collector'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let unrelated: (ProjectStatus, Option<String>) = sqlx::query_as(
        "SELECT status, container_id FROM project_services
         WHERE project_id = 'partial-failure' AND service_name = 'unrelated'",
    )
    .fetch_one(&db)
    .await
    .unwrap();

    assert_eq!(affected, (ProjectStatus::Error, None));
    assert_eq!(unrelated, (ProjectStatus::Running, Some("unrelated-container".to_string())));
    let project_status: ProjectStatus =
        sqlx::query_scalar("SELECT status FROM projects WHERE id = 'partial-failure'").fetch_one(&db).await.unwrap();
    assert_eq!(project_status, ProjectStatus::Error);
}

#[tokio::test]
async fn full_failure_marks_every_service_error() {
    let db = status_db().await;
    insert_project(&db, "full-failure", &["collector", "unrelated"]).await;

    transition(&db, "full-failure", ProjectStatus::Error, &ProjectUpdateFields::default(), None).await.unwrap();

    let statuses: Vec<ProjectStatus> = sqlx::query_scalar(
        "SELECT status FROM project_services WHERE project_id = 'full-failure' ORDER BY service_name",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(statuses, vec![ProjectStatus::Error, ProjectStatus::Error]);
}
