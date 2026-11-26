//! A project on an agent node keeps its project dir on the orchestrator too:
//! start and recreate of a remote compose project read `projects/<id>/compose.yaml`
//! there, not on the node. A copy that only reached the node failed every start
//! with "compose.yaml not found".

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum_test::TestServer;
use bytes::Bytes;
use sqlx::SqlitePool;

use crate::AppState;
use crate::copy_engine::{self, NodeRef};
use crate::tests::helpers::{build_router, build_test_state, test_config};

const COMPOSE: &str = "services:\n  web:\n    image: nginx:1.27\n";

/// Every request the stand-in saw: `(method and target, body)`.
type Requests = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// Enough of an agent for these paths: serves a compose file, records
/// project-dir imports, and fails a batch run. Listens on plain HTTP — the
/// orchestrator talks to agents that way when no CA is configured.
async fn spawn_stand_in_agent(requests: Requests) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let requests = requests.clone();
            tokio::spawn(async move {
                let _ = serve(&mut socket, &requests).await;
            });
        }
    });
    port
}

async fn serve(socket: &mut tokio::net::TcpStream, requests: &Requests) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_until(
        socket: &mut tokio::net::TcpStream,
        buf: &mut Vec<u8>,
        from: usize,
        needle: &[u8],
    ) -> std::io::Result<usize> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            if let Some(at) = buf[from..].windows(needle.len()).position(|window| window == needle) {
                return Ok(from + at + needle.len());
            }
            let read = socket.read(&mut chunk).await?;
            if read == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "request cut short"));
            }
            buf.extend_from_slice(&chunk[..read]);
        }
    }

    async fn read_through(socket: &mut tokio::net::TcpStream, buf: &mut Vec<u8>, end: usize) -> std::io::Result<()> {
        let mut chunk = [0u8; 16 * 1024];
        while buf.len() < end {
            let read = socket.read(&mut chunk).await?;
            if read == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "request cut short"));
            }
            buf.extend_from_slice(&chunk[..read]);
        }
        Ok(())
    }

    let mut buf = Vec::new();
    let head_end = read_until(socket, &mut buf, 0, b"\r\n\r\n").await?;
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let request = lines.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.trim().parse().unwrap_or(0),
                "transfer-encoding" if value.trim().eq_ignore_ascii_case("chunked") => chunked = true,
                _ => {}
            }
        }
    }

    let mut body = Vec::new();
    let mut cursor = head_end;
    if chunked {
        loop {
            let line_end = read_until(socket, &mut buf, cursor, b"\r\n").await?;
            let size_text = String::from_utf8_lossy(&buf[cursor..line_end - 2]).trim().to_string();
            let size = usize::from_str_radix(&size_text, 16)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            cursor = line_end + size + 2;
            read_through(socket, &mut buf, cursor).await?;
            body.extend_from_slice(&buf[line_end..line_end + size]);
            if size == 0 {
                break;
            }
        }
    } else {
        read_through(socket, &mut buf, head_end + content_length).await?;
        body.extend_from_slice(&buf[head_end..head_end + content_length]);
    }

    requests.lock().unwrap().push((request.clone(), body.clone()));
    let path = request.split_whitespace().nth(1).unwrap_or_default();
    let path = path.split('?').next().unwrap_or_default();
    let (status, payload) = match (request.split_whitespace().next().unwrap_or_default(), path) {
        ("GET", "/containers/compose-file") => {
            (200, serde_json::json!({ "compose_yaml": COMPOSE, "env_content": null }).to_string())
        }
        ("POST", "/volumes/import") => (200, r#"{"imported":"ok"}"#.to_string()),
        ("POST", "/containers/batch-run") => (500, r#"{"error":"stand-in node"}"#.to_string()),
        _ => (404, r#"{"error":"not served by the stand-in"}"#.to_string()),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.shutdown().await
}

async fn register_agent(state: &AppState, db: &SqlitePool, port: u16) -> String {
    let id = format!("stand-in-agent-{}", uuid::Uuid::new_v4().simple());
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO nodes (id, name, host, public_ip, agent_port, status, fail_count, created_at, updated_at) \
         VALUES (?, 'Stand-in', '127.0.0.1', '', ?, 'online', 0, ?, ?)",
    )
    .bind(&id)
    .bind(i64::from(port))
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
    state.node_clients.insert(id.clone(), Arc::new(reqwest::Client::new()));
    id
}

async fn insert_remote_compose_project(server: &TestServer, db: &SqlitePool, id: &str, node_id: &str) {
    server
        .post("/auth/register")
        .json(&serde_json::json!({"username": "tester", "password": "test-password-123"}))
        .await;
    server.post("/auth/login").json(&serde_json::json!({"username": "tester", "password": "test-password-123"})).await;
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1").fetch_one(db).await.unwrap();

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO projects (id, user_id, status, node_id, deploy_type, is_background, auto_stop_enabled, \
         auto_stop_timeout_mins, auto_start_enabled, allow_raw_ports, allow_docker_access, created_at, updated_at) \
         VALUES (?, ?, 'stopped', ?, 'compose', 0, 1, 15, 0, 0, 0, ?, ?)",
    )
    .bind(id)
    .bind(user_id)
    .bind(node_id)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO project_services (project_id, service_name, image, status, is_oneshot) \
         VALUES (?, 'web', 'nginx:1.27', 'stopped', 0)",
    )
    .bind(id)
    .execute(db)
    .await
    .unwrap();
}

fn project_dir_tar(project_id: &str) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in [
        ("compose.yaml", &b"services: {}\n"[..]),
        (".env", &b"SECRET=1\n"[..]),
        ("README.md", &b"readme"[..]),
        ("data/blob.bin", &b"bind data"[..]),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, format!("{project_id}/{name}"), data).unwrap();
    }
    builder.into_inner().unwrap()
}

/// The test project's dir on the orchestrator, removed on drop — a failing
/// assertion must not leave it in the working tree.
struct LocalProjectDir(std::path::PathBuf);

impl Drop for LocalProjectDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn project_dir_sent_to_an_agent_is_kept_on_the_orchestrator() {
    let (state, db) = build_test_state(test_config()).await;
    let requests: Requests = Arc::default();
    let node = register_agent(&state, &db, spawn_stand_in_agent(requests.clone()).await).await;
    let id = format!("remote-copy-{}", uuid::Uuid::new_v4().simple());
    let local_dir = LocalProjectDir(std::path::PathBuf::from("projects").join(&id));
    let tar = project_dir_tar(&id);
    let sent = tar.clone();

    copy_engine::import_project_dir(
        &state,
        &NodeRef::Agent(node),
        &id,
        Box::pin(futures_util::stream::once(async move { Bytes::from(sent) })),
        &std::env::temp_dir(),
    )
    .await
    .unwrap();

    // The node received the whole dir, `.env` included…
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].0.starts_with("POST /volumes/import?kind=project-dir"));
    assert!(requests[0].0.contains(&format!("name={id}")), "import was not for {id}: {}", requests[0].0);
    assert_eq!(requests[0].1, tar, "the node did not receive the archive as sent");
    drop(requests);
    // …and the orchestrator's copy is the top level minus `.env` and bind data.
    assert_eq!(std::fs::read_to_string(local_dir.0.join("compose.yaml")).unwrap(), "services: {}\n");
    assert!(local_dir.0.join("README.md").exists());
    assert!(!local_dir.0.join(".env").exists());
    assert!(!local_dir.0.join("data").exists());
}

#[tokio::test]
async fn remote_start_takes_a_missing_compose_file_from_the_node() {
    let (state, db) = build_test_state(test_config()).await;
    let server = TestServer::new(build_router(state.clone())).unwrap();
    let requests: Requests = Arc::default();
    let node = register_agent(&state, &db, spawn_stand_in_agent(requests.clone()).await).await;
    let id = format!("remote-start-{}", uuid::Uuid::new_v4().simple());
    let local_dir = LocalProjectDir(std::path::PathBuf::from("projects").join(&id));
    assert!(!local_dir.0.exists(), "the orchestrator starts without a copy");
    insert_remote_compose_project(&server, &db, &id, &node).await;

    // The stand-in fails the batch run, so the start gets exactly as far as the compose file.
    let response = crate::routes::manage::handlers::start_project_route(State(state.clone()), Path(id.clone())).await;
    let (parts, body) = response.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    let error = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"].as_str().unwrap().to_string();
    assert_eq!(parts.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.contains("remote start failed"), "got past the compose file: {error}");
    assert_eq!(std::fs::read_to_string(local_dir.0.join("compose.yaml")).unwrap(), COMPOSE);
    let project_id = id.clone();
    let asked_the_node = |requests: &Requests| {
        requests.lock().unwrap().iter().any(|(request, _)| {
            request.starts_with("GET /containers/compose-file?dir=") && request.contains(&project_id)
        })
    };
    assert!(asked_the_node(&requests));

    // The kept copy is what the next start reads.
    requests.lock().unwrap().clear();
    let _ = crate::routes::manage::handlers::start_project_route(State(state), Path(id)).await;
    assert!(!asked_the_node(&requests));
}
