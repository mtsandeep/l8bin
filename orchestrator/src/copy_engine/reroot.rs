//! Project-dir tar re-rooting.
//!
//! `TransferKind::ProjectDir` tars carry the project id as the top-level
//! directory (`{old_id}/compose.yaml`, …) and the import side keeps that
//! name, so re-instantiating under a different id requires rewriting the
//! archive. The stream is spooled to a temp file and rewritten with the sync
//! `tar` crate in a blocking task — project dirs are small (the big payloads
//! are volumes, which rename for free via the target-name param).

use std::path::Path;
use std::pin::Pin;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio_util::io::StreamReader;

type ByteStream = Pin<Box<dyn Stream<Item = Bytes> + Send>>;

/// Rewrite a project-dir tar, replacing the top-level `old_id/` component
/// with `new_id/`. Entries outside `old_id/` are rejected — this doubles as
/// the path-traversal guard. `.env.l8bin` is always dropped; all other
/// top-level `.env*` files are dropped when `include_env` is false.
pub async fn reroot(
    tar: ByteStream,
    old_id: &str,
    new_id: &str,
    include_env: bool,
    tmp_dir: &Path,
) -> anyhow::Result<ByteStream> {
    tokio::fs::create_dir_all(tmp_dir).await?;
    let unique = uuid::Uuid::new_v4().simple().to_string();
    let in_path = tmp_dir.join(format!("reroot-{unique}.in.tar"));
    let out_path = tmp_dir.join(format!("reroot-{unique}.out.tar"));

    let result = reroot_inner(tar, old_id, new_id, include_env, &in_path, &out_path).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&in_path).await;
        let _ = tokio::fs::remove_file(&out_path).await;
    }
    result
}

async fn reroot_inner(
    tar: ByteStream,
    old_id: &str,
    new_id: &str,
    include_env: bool,
    in_path: &Path,
    out_path: &Path,
) -> anyhow::Result<ByteStream> {
    // 1. Spool the incoming stream.
    let mut reader = StreamReader::new(tar.map(std::io::Result::Ok));
    {
        let mut file = tokio::fs::File::create(in_path).await?;
        tokio::io::copy(&mut reader, &mut file).await?;
    }

    // 2. Rewrite off the async runtime.
    let (old, new) = (old_id.to_string(), new_id.to_string());
    let (in_p, out_p) = (in_path.to_path_buf(), out_path.to_path_buf());
    tokio::task::spawn_blocking(move || rewrite_sync(&in_p, &out_p, &old, &new, include_env)).await??;
    let _ = tokio::fs::remove_file(in_path).await;

    // 3. Stream the result, deleting the spool once consumed.
    let file = tokio::fs::File::open(out_path).await?;
    let cleanup = out_path.to_path_buf();
    let stream = tokio_util::io::ReaderStream::new(file).map(|chunk| chunk.unwrap_or_default()).chain(
        futures_util::stream::once(async move {
            let _ = tokio::fs::remove_file(cleanup).await;
            Bytes::new()
        }),
    );
    Ok(Box::pin(stream))
}

fn rewrite_sync(in_path: &Path, out_path: &Path, old_id: &str, new_id: &str, include_env: bool) -> anyhow::Result<()> {
    let mut archive = tar::Archive::new(std::fs::File::open(in_path)?);
    let mut builder = tar::Builder::new(std::fs::File::create(out_path)?);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let path_str = path.to_string_lossy();
        let normalized = path_str.strip_prefix("./").unwrap_or(&path_str);
        let rest = Path::new(normalized)
            .strip_prefix(old_id)
            .map_err(|_| anyhow::anyhow!("project-dir tar entry '{normalized}' is outside '{old_id}/'"))?;

        if rest.components().count() == 1 {
            let name = rest.to_string_lossy();
            if name == ".env.l8bin" || (!include_env && name.starts_with(".env")) {
                continue;
            }
        }

        // append_data re-encodes the path (pax extensions for long names).
        let mut header = entry.header().clone();
        let new_path = Path::new(new_id).join(rest);
        builder.append_data(&mut header, &new_path, &mut entry)?;
    }
    builder.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream::iter as stream_iter;

    fn tar_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, data.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap()
    }

    async fn collect(stream: ByteStream) -> Vec<u8> {
        let mut out = Vec::new();
        futures_util::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk);
        }
        out
    }

    fn entry_paths(bytes: &[u8]) -> Vec<String> {
        tar::Archive::new(std::io::Cursor::new(bytes.to_vec()))
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn tmp() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("litebin-reroot-test-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn reroots_top_level_dir_and_filters_env_snapshot() {
        let bytes = tar_bytes(&[
            ("old/compose.yaml", "services: {}"),
            ("old/.env", "SECRET=1"),
            ("old/.env.l8bin", "SECRET=1"),
            ("old/data/file.txt", "x"),
        ]);
        let stream: ByteStream = Box::pin(stream_iter(vec![Bytes::from(bytes)]));
        let out = collect(reroot(stream, "old", "new", true, &tmp()).await.unwrap()).await;
        assert_eq!(
            entry_paths(&out),
            vec!["new/compose.yaml".to_string(), "new/.env".to_string(), "new/data/file.txt".to_string()]
        );
    }

    #[tokio::test]
    async fn excludes_env_when_asked() {
        let bytes = tar_bytes(&[("old/.env", "SECRET=1"), ("old/.env.l8bin", "SECRET=1"), ("old/compose.yaml", "x")]);
        let stream: ByteStream = Box::pin(stream_iter(vec![Bytes::from(bytes)]));
        let out = collect(reroot(stream, "old", "new", false, &tmp()).await.unwrap()).await;
        assert_eq!(entry_paths(&out), vec!["new/compose.yaml".to_string()]);
    }

    #[tokio::test]
    async fn rejects_entries_outside_old_id() {
        // `..` / absolute paths land in the same branch — the tar crate refuses
        // to write those at all, so a different top-level dir stands in.
        let bytes = tar_bytes(&[("old/compose.yaml", "x"), ("other/file.txt", "evil")]);
        let stream: ByteStream = Box::pin(stream_iter(vec![Bytes::from(bytes)]));
        assert!(reroot(stream, "old", "new", true, &tmp()).await.is_err());
    }

    #[tokio::test]
    async fn same_id_no_env_skip_still_strips_snapshot() {
        // duplicate always reroots when include_env=false, even same-id paths.
        let bytes = tar_bytes(&[("app/.env", "S=1"), ("app/.env.l8bin", "S=1"), ("app/a.txt", "x")]);
        let stream: ByteStream = Box::pin(stream_iter(vec![Bytes::from(bytes)]));
        let out = collect(reroot(stream, "app", "app", false, &tmp()).await.unwrap()).await;
        assert_eq!(entry_paths(&out), vec!["app/a.txt".to_string()]);
    }
}
