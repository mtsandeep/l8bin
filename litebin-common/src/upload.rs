//! Shared chunked, resumable upload primitives used by both the orchestrator
//! (local + relay uploads) and the agent (direct uploads).
//!
//! A client splits an image tar into fixed-size chunks and POSTs them to
//! `{base}/{token}/chunk/{index}`. The server stages each chunk to disk under a
//! directory keyed deterministically by `(project_id, image_id)`, so a re-minted
//! token or a restarted client resumes the same partial upload. `commit`
//! concatenates the chunks in order and either loads them into local Docker
//! (local/direct) or streams them to a remote agent's `/images/load` (relay).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use futures_util::Stream;
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::docker::DockerManager;

/// Default chunk size: 32 MiB. Large enough to keep request count sane on a
/// 894 MiB image (~28 chunks), small enough that a dropped chunk is cheap to resend.
pub const DEFAULT_CHUNK_SIZE: usize = 32 * 1024 * 1024;

/// Default session lifetime: 90 minutes — comfortably longer than the slowest
/// expected upload, short enough to bound orphaned staging.
pub const DEFAULT_TTL_SECS: i64 = 90 * 60;

/// Max accepted request body for chunk uploads (3× the chunk size, so a full chunk
/// plus headroom always fits under axum's body limit).
pub const MAX_UPLOAD_BODY: usize = DEFAULT_CHUNK_SIZE * 3;

// ── Protocol surface: paths, segments, header ───────────────────────────────
// All upload-related paths live here so the contract between client, master, and
// agent is discoverable in one place. Route registration (server) and URL
// construction (client) both derive from these.

/// URL prefix for chunk requests served by the agent (direct uploads), reached via
/// the agent Caddy on `:443`.
pub const AGENT_UPLOAD_PREFIX: &str = "/__l8b_upload";
/// URL prefix for chunk requests served by the master (local + relay uploads).
pub const MASTER_UPLOAD_PREFIX: &str = "/images/upload";
/// Agent route (called by the master over mTLS) that mints a direct-upload token.
pub const MINT_PATH: &str = "/internal/mint-upload-token";

/// Sub-path segment naming the status/chunk/commit operations.
pub const SEG_STATUS: &str = "status";
pub const SEG_CHUNK: &str = "chunk";
pub const SEG_COMMIT: &str = "commit";

/// Request header carrying the total chunk count on each chunk POST.
pub const TOTAL_CHUNKS_HEADER: &str = "x-total-chunks";

/// Build a server-side route pattern (`{prefix}/{token}/{seg}`) for axum registration.
fn route_pattern(prefix: &str, seg: &str) -> String {
    format!("{prefix}/{{token}}/{seg}")
}

/// `…/chunk/{index}` route pattern.
fn chunk_route_pattern(prefix: &str) -> String {
    format!("{prefix}/{{token}}/{SEG_CHUNK}/{{index}}")
}

/// Route patterns for the agent's loopback upload server.
pub fn agent_status_route() -> String {
    route_pattern(AGENT_UPLOAD_PREFIX, SEG_STATUS)
}
pub fn agent_chunk_route() -> String {
    chunk_route_pattern(AGENT_UPLOAD_PREFIX)
}
pub fn agent_commit_route() -> String {
    route_pattern(AGENT_UPLOAD_PREFIX, SEG_COMMIT)
}

/// Route patterns for the master's chunk endpoints.
pub fn master_status_route() -> String {
    route_pattern(MASTER_UPLOAD_PREFIX, SEG_STATUS)
}
pub fn master_chunk_route() -> String {
    chunk_route_pattern(MASTER_UPLOAD_PREFIX)
}
pub fn master_commit_route() -> String {
    route_pattern(MASTER_UPLOAD_PREFIX, SEG_COMMIT)
}

/// Client URL builders (`{base}/{token}/{seg}`), where `base` is the prefix the
/// broker returned (agent public URL for direct, master server for local/relay).
pub fn status_url(base: &str, token: &str) -> String {
    format!("{base}/{token}/{SEG_STATUS}")
}
pub fn chunk_url(base: &str, token: &str, index: u64) -> String {
    format!("{base}/{token}/{SEG_CHUNK}/{index}")
}
pub fn commit_url(base: &str, token: &str) -> String {
    format!("{base}/{token}/{SEG_COMMIT}")
}

/// Read the `X-Total-Chunks` header from a request.
pub fn total_chunks_header(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(TOTAL_CHUNKS_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// Sort a chunk-index set into a stable ascending Vec for JSON responses.
pub fn sorted_indices(set: HashSet<u64>) -> Vec<u64> {
    let mut v: Vec<u64> = set.into_iter().collect();
    v.sort_unstable();
    v
}

// ── Protocol request/response types ─────────────────────────────────────────

/// `POST /internal/mint-upload-token` body (master → agent).
#[derive(Debug, Serialize, Deserialize)]
pub struct MintRequest {
    pub project_id: String,
    pub image_id: String,
    pub node_id: String,
    #[serde(default)]
    pub ttl_secs: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MintResponse {
    pub token: String,
    pub expires_at: i64,
    pub chunk_size: u64,
}

/// `GET …/status` response.
#[derive(Debug, Serialize)]
pub struct UploadStatusResponse {
    pub received: Vec<u64>,
    pub total: Option<u64>,
    pub expires_at: i64,
    pub chunk_size: u64,
}

/// `POST …/chunk/{index}` response.
#[derive(Debug, Serialize)]
pub struct UploadChunkResponse {
    pub received: Vec<u64>,
    pub total: Option<u64>,
}

/// `POST …/commit` response.
#[derive(Debug, Serialize)]
pub struct UploadCommitResponse {
    pub image_id: String,
}

/// In-memory record of an in-progress (or completed) upload session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadSession {
    pub token: String,
    pub project_id: String,
    pub image_id: String,
    pub node_id: String,
    /// Chunk indices present on disk.
    pub received: HashSet<u64>,
    /// Total chunk count, learned from the client via the `X-Total-Chunks` header.
    pub total: Option<u64>,
    pub committed: bool,
    pub created_at: i64,
    pub expires_at: i64,
}

impl UploadSession {
    pub fn is_expired(&self, now: i64) -> bool {
        now >= self.expires_at
    }
}

/// Resolved, validated commit context returned by [`UploadStore::prepare_commit`].
#[derive(Debug)]
pub struct PreparedCommit {
    pub dir: PathBuf,
    pub total: u64,
    pub project_id: String,
    pub image_id: String,
    pub node_id: String,
}

/// Errors that map cleanly to HTTP responses in the handlers.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("unknown or expired upload token")]
    NotFound,
    #[error("upload session expired")]
    Expired,
    #[error("upload session already committed")]
    AlreadyCommitted,
    #[error("missing chunks: {0}")]
    MissingChunks(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl UploadError {
    /// HTTP status this error maps to. Shared so the agent and master stay consistent.
    pub fn status(&self) -> http::StatusCode {
        use http::StatusCode;
        match self {
            UploadError::NotFound => StatusCode::NOT_FOUND,
            UploadError::Expired => StatusCode::GONE,
            UploadError::AlreadyCommitted | UploadError::MissingChunks(_) => StatusCode::CONFLICT,
            UploadError::Io(_) | UploadError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// In-memory store of upload sessions, backed by on-disk chunk staging.
///
/// `staging_root` is created lazily. Sessions are keyed by token in memory; the
/// on-disk staging dir is keyed deterministically by `(project_id, image_id)` so
/// a re-minted token (e.g. after the previous one expired) resumes the same
/// partial upload — the `received` set is rebuilt from disk at mint time.
pub struct UploadStore {
    sessions: Arc<DashMap<String, UploadSession>>,
    staging_root: PathBuf,
    chunk_size: u64,
}

impl UploadStore {
    pub fn new<P: Into<PathBuf>>(staging_root: P, chunk_size: usize) -> std::io::Result<Self> {
        let staging_root = staging_root.into();
        std::fs::create_dir_all(&staging_root)?;
        Ok(Self {
            sessions: Arc::new(DashMap::new()),
            staging_root,
            chunk_size: chunk_size as u64,
        })
    }

    pub fn chunk_size(&self) -> u64 {
        self.chunk_size
    }

    /// Mint a new session. If staging for `(project_id, image_id)` already exists
    /// (e.g. a prior attempt), `received` is rebuilt from disk so the client
    /// resumes instead of restarting.
    pub fn mint(
        &self,
        project_id: &str,
        image_id: &str,
        node_id: &str,
        ttl_secs: i64,
    ) -> Result<UploadSession, UploadError> {
        let now = chrono::Utc::now().timestamp();
        let staging_dir = self.session_dir(project_id, image_id);
        std::fs::create_dir_all(&staging_dir)?;
        let received = scan_received(&staging_dir)?;

        // 32 random bytes → hex (64 chars). Opaque, unforgeable, URL-safe.
        let token_bytes: [u8; 32] = rand::random();
        let token = hex::encode(token_bytes);

        let session = UploadSession {
            token: token.clone(),
            project_id: project_id.to_string(),
            image_id: image_id.to_string(),
            node_id: node_id.to_string(),
            received,
            total: None,
            committed: false,
            created_at: now,
            expires_at: now + ttl_secs.max(60),
        };
        self.sessions.insert(token.clone(), session.clone());
        Ok(session)
    }

    /// Look up a session and enforce validity (exists, not expired, not committed).
    fn live(&self, token: &str) -> Result<dashmap::mapref::one::Ref<'_, String, UploadSession>, UploadError> {
        let s = self.sessions.get(token).ok_or(UploadError::NotFound)?;
        if s.committed {
            return Err(UploadError::AlreadyCommitted);
        }
        if s.is_expired(chrono::Utc::now().timestamp()) {
            return Err(UploadError::Expired);
        }
        Ok(s)
    }

    pub fn status(&self, token: &str) -> Result<(HashSet<u64>, Option<u64>, i64), UploadError> {
        let s = self.live(token)?;
        Ok((s.received.clone(), s.total, s.expires_at))
    }

    /// Record a chunk: persist bytes to disk and update the session. Idempotent —
    /// re-uploading an index overwrites the file and is a no-op on the set.
    pub fn write_chunk(
        &self,
        token: &str,
        index: u64,
        total: Option<u64>,
        bytes: Bytes,
    ) -> Result<(HashSet<u64>, Option<u64>), UploadError> {
        // Validate first so we don't write bytes for an invalid session.
        {
            let s = self.live(token)?;
            let dir = self.session_dir(&s.project_id, &s.image_id);
            std::fs::create_dir_all(&dir)?;
            let path = chunk_file(&dir, index);
            std::fs::write(&path, &bytes[..])?;
        }
        // Re-acquire and update totals.
        let mut s = self.live(token)?.clone();
        drop(self.sessions.remove(token));
        if let Some(t) = total {
            s.total = Some(t);
        }
        s.received.insert(index);
        let received = s.received.clone();
        let total = s.total;
        self.sessions.insert(token.to_string(), s);
        Ok((received, total))
    }

    /// Validate that every chunk `0..total` is present; returns everything the
    /// caller needs to load/stream + route the commit. Does not mark committed
    /// (the caller does, after a successful load/stream).
    pub fn prepare_commit(&self, token: &str) -> Result<PreparedCommit, UploadError> {
        let s = self.live(token)?.clone();
        let total = s.total.ok_or_else(|| UploadError::Other(anyhow::anyhow!("total chunk count unknown; send X-Total-Chunks")))?;
        let missing: Vec<u64> = (0..total).filter(|i| !s.received.contains(i)).collect();
        if !missing.is_empty() {
            return Err(UploadError::MissingChunks(format!("{:?}", missing)));
        }
        Ok(PreparedCommit {
            dir: self.session_dir(&s.project_id, &s.image_id),
            total,
            project_id: s.project_id,
            image_id: s.image_id,
            node_id: s.node_id,
        })
    }

    pub fn mark_committed(&self, token: &str) {
        if let Some(mut s) = self.sessions.get_mut(token) {
            s.committed = true;
        }
    }

    /// Remove a session and its staging dir (best-effort). Called after a
    /// successful commit or when abandoning a session.
    pub fn purge(&self, token: &str) {
        if let Some((_, s)) = self.sessions.remove(token) {
            let dir = self.session_dir(&s.project_id, &s.image_id);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Evict expired sessions and orphaned staging dirs. Intended to be called
    /// periodically from a background task.
    pub fn gc(&self) {
        let now = chrono::Utc::now().timestamp();
        let expired: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| s.is_expired(now))
            .map(|s| s.token.clone())
            .collect();
        for token in expired {
            self.purge(&token);
        }
        // Also sweep staging dirs with no live session (e.g. left by a crash).
        if let Ok(entries) = std::fs::read_dir(&self.staging_root) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    let stale = !self.sessions.iter().any(|s| session_dir_name(&s.project_id, &s.image_id) == name);
                    if stale {
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                }
            }
        }
    }

    fn session_dir(&self, project_id: &str, image_id: &str) -> PathBuf {
        self.staging_root.join(session_dir_name(project_id, image_id))
    }
}

/// Deterministic per-(project, image) dir name so re-mints resume the same staging.
fn session_dir_name(project_id: &str, image_id: &str) -> String {
    let slug = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect::<String>();
    format!("{}-{}", slug(project_id), slug(image_id))
}

fn chunk_file(dir: &Path, index: u64) -> PathBuf {
    dir.join(format!("{:08}", index))
}

/// Scan a staging dir for chunk files present, returning their indices.
fn scan_received(dir: &Path) -> Result<HashSet<u64>, UploadError> {
    let mut set = HashSet::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(index) = name.parse::<u64>() {
                    set.insert(index);
                }
            }
        }
    }
    Ok(set)
}

/// Build a byte stream that yields the chunks `0..total` in order, reading each
/// chunk file sequentially. Used to feed `DockerManager::load_image` (local/direct)
/// or to wrap as a `reqwest::Body` for relay streaming — one implementation, both uses.
///
/// Boxed + pinned so it is `Unpin` (required by `load_image`).
pub fn chunk_stream(
    dir: PathBuf,
    total: u64,
) -> impl Stream<Item = std::result::Result<Bytes, std::io::Error>> + Send + Unpin + 'static {
    Box::pin(async_stream::stream! {
        for index in 0..total {
            let path = chunk_file(&dir, index);
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(f) => f,
                Err(e) => {
                    yield Err(e);
                    break;
                }
            };
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match file.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => yield Ok(Bytes::copy_from_slice(&buf[..n])),
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                }
            }
        }
    })
}

/// Load concatenated chunks `0..total` from staging into local Docker and resolve
/// the resulting image id. Used for local-node and direct-to-agent commits.
pub async fn load_staged(
    docker: &DockerManager,
    dir: PathBuf,
    total: u64,
    image_id: &str,
) -> anyhow::Result<String> {
    let stream = chunk_stream(dir, total);
    docker.load_image(stream).await?;
    docker.inspect_image_id(image_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> UploadStore {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("l8b-upload-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        UploadStore::new(&dir, 1024).unwrap()
    }

    #[test]
    fn mint_creates_session_and_rebuilds_from_disk_on_re_mint() {
        let store = temp_store();
        let s1 = store.mint("proj", "l8b/proj-web", "node1", 60).unwrap();
        // Write chunks 0 and 2 (out of order, skipping 1).
        store.write_chunk(&s1.token, 0, Some(3), Bytes::from_static(&[1; 5])).unwrap();
        store.write_chunk(&s1.token, 2, Some(3), Bytes::from_static(&[2; 5])).unwrap();

        let (received, total, _) = store.status(&s1.token).unwrap();
        assert_eq!(total, Some(3));
        assert_eq!(received, [0, 2].into_iter().collect::<HashSet<_>>());

        // Re-mint for the same (project, image) must resume from disk.
        let s2 = store.mint("proj", "l8b/proj-web", "node1", 60).unwrap();
        assert_ne!(s1.token, s2.token, "re-mint should issue a fresh token");
        assert_eq!(s2.received, [0, 2].into_iter().collect::<HashSet<_>>(), "received rebuilt from disk");
    }

    #[test]
    fn chunk_write_is_idempotent() {
        let store = temp_store();
        let s = store.mint("p", "img", "n", 60).unwrap();
        store.write_chunk(&s.token, 1, Some(4), Bytes::from_static(&[9; 7])).unwrap();
        // Overwriting the same index is a no-op on the received set.
        store.write_chunk(&s.token, 1, Some(4), Bytes::from_static(&[9; 7])).unwrap();
        let (received, total, _) = store.status(&s.token).unwrap();
        assert_eq!(received.len(), 1);
        assert!(received.contains(&1));
        assert_eq!(total, Some(4));
    }

    #[test]
    fn prepare_commit_rejects_when_chunks_missing() {
        let store = temp_store();
        let s = store.mint("p", "img", "n", 60).unwrap();
        store.write_chunk(&s.token, 0, Some(3), Bytes::from_static(&[0; 3])).unwrap();
        store.write_chunk(&s.token, 2, Some(3), Bytes::from_static(&[0; 3])).unwrap();
        let err = store.prepare_commit(&s.token).unwrap_err();
        assert!(matches!(err, UploadError::MissingChunks(_)), "got {err:?}");
    }

    #[test]
    fn prepare_commit_succeeds_when_complete() {
        let store = temp_store();
        let s = store.mint("p", "img", "local", 60).unwrap();
        for i in 0..3 {
            store.write_chunk(&s.token, i, Some(3), Bytes::from(vec![i as u8 + 1; 4])).unwrap();
        }
        let prep = store.prepare_commit(&s.token).unwrap();
        assert_eq!(prep.total, 3);
        assert_eq!(prep.node_id, "local");
    }

    #[test]
    fn unknown_and_committed_tokens_rejected() {
        let store = temp_store();
        assert!(matches!(store.status("nope").unwrap_err(), UploadError::NotFound));

        let s = store.mint("p", "img", "n", 60).unwrap();
        store.mark_committed(&s.token);
        assert!(matches!(store.status(&s.token).unwrap_err(), UploadError::AlreadyCommitted));
    }

    #[test]
    fn purge_removes_staging() {
        let store = temp_store();
        let s = store.mint("p", "img", "n", 60).unwrap();
        store.write_chunk(&s.token, 0, Some(1), Bytes::from_static(&[1; 3])).unwrap();
        let dir = store.session_dir("p", "img");
        assert!(dir.exists());
        store.purge(&s.token);
        assert!(!dir.exists());
        assert!(store.status(&s.token).is_err());
    }
}
