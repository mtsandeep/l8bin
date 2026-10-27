//! Export bundle writer — a gzip'd tar holding the manifest plus payload tars.
//!
//! Payload streams (project dir, volumes, images) come from the transfer
//! engine with unknown lengths, but tar headers need the size up front, so
//! the caller spools each payload to a temp file first; the assembler
//! appends from disk and deletes the spool as it goes (peak extra disk is
//! one payload, not the whole project). Assembly runs in a blocking task
//! fed by a channel; gzip level 1 wraps the output.

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const MANIFEST_ENTRY: &str = "manifest.json";
pub const PROJECT_ENTRY: &str = "project.tar";

/// Write the bundle to `out_path` (as `out_path.part`, renamed on success).
///
/// `next_payload` spools and yields one `(entry_name, spool_path)` at a time,
/// ending with `None`. Spools are deleted by the writer after being appended.
pub async fn write_bundle<F, Fut>(out_path: &Path, manifest_json: Vec<u8>, mut next_payload: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<Option<(String, PathBuf)>>>,
{
    let mut part = out_path.as_os_str().to_os_string();
    part.push(".part");
    let part = PathBuf::from(part);

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<(String, PathBuf)>();
    let assembler = {
        let part = part.clone();
        let out = out_path.to_path_buf();
        tokio::task::spawn_blocking(move || assemble(&part, &out, &manifest_json, rx))
    };

    while let Some(payload) = next_payload().await? {
        if tx.send(payload).is_err() {
            break; // assembler died; its error surfaces on join
        }
    }
    drop(tx);

    assembler.await??;
    Ok(())
}

fn assemble(
    part: &Path,
    out_path: &Path,
    manifest_json: &[u8],
    mut rx: tokio::sync::mpsc::UnboundedReceiver<(String, PathBuf)>,
) -> anyhow::Result<()> {
    let file = std::fs::File::create(part)?;
    let buf = std::io::BufWriter::with_capacity(1024 * 1024, file);
    let gz = flate2::write::GzEncoder::new(buf, flate2::Compression::new(1));
    let mut builder = tar::Builder::new(gz);

    append_bytes(&mut builder, MANIFEST_ENTRY, manifest_json)?;

    while let Some((name, spool)) = rx.blocking_recv() {
        let result = (|| -> std::io::Result<()> {
            let meta = std::fs::metadata(&spool)?;
            let mut header = tar::Header::new_gnu();
            header.set_size(meta.len());
            header.set_mode(0o600);
            header.set_cksum();
            builder.append_data(&mut header, &name, std::fs::File::open(&spool)?)
        })();
        let _ = std::fs::remove_file(&spool);
        result?;
    }

    builder.finish()?;
    let gz = builder.into_inner()?;
    let mut buf = gz.finish()?;
    buf.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(part, std::fs::Permissions::from_mode(0o600));
    }
    let file = buf.get_ref();
    file.sync_all()?;
    std::fs::rename(part, out_path)?;
    Ok(())
}

fn append_bytes(builder: &mut tar::Builder<impl Write>, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    builder.append_data(&mut header, name, bytes)?;
    Ok(())
}

// ── Reader ───────────────────────────────────────────────────────────────────

/// Open a bundle (gzip sniffed — plain tar also accepted) and return the
/// manifest. Fails fast on foreign archives: the manifest must be present and
/// carry the expected format marker.
pub async fn read_manifest(path: &std::path::Path) -> anyhow::Result<super::manifest::ExportManifest> {
    let p = path.to_path_buf();
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let file = std::fs::File::open(&p)?;
        let mut head = [0u8; 2];
        use std::io::Read;
        let mut probe = std::io::BufReader::new(&file);
        let n = probe.read(&mut head)?;
        let reader: Box<dyn std::io::Read> = if n == 2 && head == [0x1f, 0x8b] {
            Box::new(flate2::read::GzDecoder::new(std::fs::File::open(&p)?))
        } else {
            Box::new(std::fs::File::open(&p)?)
        };
        for entry in tar::Archive::new(reader).entries()? {
            let mut entry = entry?;
            if entry.path()?.to_string_lossy() == MANIFEST_ENTRY {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                return Ok(buf);
            }
        }
        anyhow::bail!("no {MANIFEST_ENTRY} entry — not a LiteBin export bundle")
    })
    .await??;

    let manifest: super::manifest::ExportManifest =
        serde_json::from_slice(&bytes).map_err(|e| anyhow::anyhow!("manifest parse failed: {e}"))?;
    if manifest.format != super::manifest::MANIFEST_FORMAT {
        anyhow::bail!("unsupported bundle format '{}'", manifest.format);
    }
    Ok(manifest)
}

/// Stream every payload entry (skipping the manifest) through `on_entry`,
/// spooling one at a time to `tmp_dir` (peak extra disk = largest payload).
/// `on_entry` receives `(entry_name, spool_path)` and owns deletion of the spool.
pub async fn iterate_entries<F, Fut>(
    path: &std::path::Path,
    tmp_dir: &std::path::Path,
    mut on_entry: F,
) -> anyhow::Result<()>
where
    F: FnMut(String, PathBuf) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    use std::io::Read;

    tokio::fs::create_dir_all(tmp_dir).await?;
    let (tx, rx) = std::sync::mpsc::channel::<anyhow::Result<(String, PathBuf)>>();
    let p = path.to_path_buf();
    let tmp = tmp_dir.to_path_buf();
    let reader_task = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let file = std::fs::File::open(&p)?;
        let mut head = [0u8; 2];
        let mut probe = std::io::BufReader::new(&file);
        let n = probe.read(&mut head)?;
        let reader: Box<dyn std::io::Read> = if n == 2 && head == [0x1f, 0x8b] {
            Box::new(flate2::read::GzDecoder::new(std::fs::File::open(&p)?))
        } else {
            Box::new(std::fs::File::open(&p)?)
        };
        for entry in tar::Archive::new(reader).entries()? {
            let mut entry = match entry {
                Ok(e) => e,
                Err(e) => return Err(e.into()),
            };
            let name = entry.path()?.to_string_lossy().into_owned();
            if name == MANIFEST_ENTRY {
                continue;
            }
            // Spool file names are sanitized; which entries are expected (and
            // where they go) is decided by the import dispatch, not here.
            let spool = tmp.join(format!("entry-{}-{}", uuid::Uuid::new_v4().simple(), sanitize_entry_name(&name)));
            let mut out = std::fs::File::create(&spool)?;
            std::io::copy(&mut entry, &mut out)?;
            drop(out);
            let spool_path = spool.clone();
            if tx.send(Ok((name, spool))).is_err() {
                let _ = std::fs::remove_file(&spool_path);
                anyhow::bail!("import consumer stopped early");
            }
        }
        Ok(())
    });

    let mut consumer_error: Option<anyhow::Error> = None;
    while let Ok(item) = rx.recv() {
        match item {
            Ok((name, spool)) => {
                if let Err(e) = on_entry(name, spool).await {
                    consumer_error = Some(e);
                    break;
                }
            }
            Err(e) => {
                consumer_error = Some(e);
                break;
            }
        }
    }
    drop(rx);

    // Join the reader; its error matters unless the consumer already failed.
    if let Err(e) = reader_task.await?
        && consumer_error.is_none()
    {
        return Err(e);
    }
    match consumer_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn sanitize_entry_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '-' })
        .collect();
    cleaned.chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_manifest() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "format": "litebin.export",
            "version": 1,
            "created_at": 1,
            "source": { "master_version": "t", "node_id": "local", "architecture": "x86_64" },
            "project": {
                "id": "my-app", "name": null, "description": null, "is_background": false,
                "image": null, "internal_port": null, "cmd": null, "memory_limit_mb": null,
                "cpu_limit": null, "auto_stop_enabled": true, "auto_stop_timeout_mins": 15,
                "auto_start_enabled": false, "allow_raw_ports": false, "allow_docker_access": false,
                "volumes": null, "deploy_type": "compose"
            },
            "services": [],
            "volumes": [],
            "images": [],
            "flags": { "include_env": true, "include_volumes": true }
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn reads_manifest_and_iterates_entries() {
        let dir = std::env::temp_dir().join(format!("litebin-bundle-read-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("x.l8b.tar.gz");
        let spool1 = dir.join("a.tar");
        let spool2 = dir.join("b.tar");
        std::fs::write(&spool1, b"project").unwrap();
        std::fs::write(&spool2, b"volume").unwrap();
        let mut pending = vec![
            (PROJECT_ENTRY.to_string(), spool1.clone()),
            ("volumes/litebin_x_data.tar".to_string(), spool2.clone()),
        ];
        pending.reverse();
        write_bundle(&out, minimal_manifest(), move || {
            let next = pending.pop();
            async move { Ok(next) }
        })
        .await
        .unwrap();

        let manifest = read_manifest(&out).await.unwrap();
        assert_eq!(manifest.project.id, "my-app");

        let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        iterate_entries(&out, &dir.join("tmp"), move |name, spool| {
            let seen = seen_clone.clone();
            async move {
                let content = std::fs::read_to_string(&spool).unwrap_or_default();
                std::fs::remove_file(&spool).unwrap();
                seen.lock().unwrap().push((name, content));
                Ok(())
            }
        })
        .await
        .unwrap();

        let got = seen.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                (PROJECT_ENTRY.to_string(), "project".to_string()),
                ("volumes/litebin_x_data.tar".to_string(), "volume".to_string()),
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_manifest_rejects_foreign_tars() {
        let dir = std::env::temp_dir().join(format!("litebin-bundle-foreign-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain.tar");
        let mut builder = tar::Builder::new(std::fs::File::create(&plain).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_size(2);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, "random.txt", &b"hi"[..]).unwrap();
        builder.finish().unwrap();
        assert!(read_manifest(&plain).await.is_err(), "no manifest entry → rejected");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn writes_readable_gzip_tar_and_deletes_spools() {
        let dir = std::env::temp_dir().join(format!("litebin-bundle-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("x.l8b.tar.gz");

        let spool1 = dir.join("p1.tar");
        let spool2 = dir.join("p2.tar");
        std::fs::write(&spool1, b"project-dir-tar-bytes").unwrap();
        std::fs::write(&spool2, b"volume-tar-bytes").unwrap();

        let manifest = br#"{"format":"litebin.export","version":1}"#;
        let mut pending = vec![
            (PROJECT_ENTRY.to_string(), spool1.clone()),
            ("volumes/litebin_x_data.tar".to_string(), spool2.clone()),
        ];
        pending.reverse();

        write_bundle(&out, manifest.to_vec(), move || {
            let next = pending.pop();
            async move { Ok(next) }
        })
        .await
        .unwrap();

        assert!(out.exists());
        assert!(!spool1.exists(), "spool deleted after append");
        assert!(!spool2.exists(), "spool deleted after append");
        assert!(!dir.join("x.l8b.tar.gz.part").exists());

        // Read back: gzip magic, then entry names and contents in order.
        let raw = std::fs::read(&out).unwrap();
        assert_eq!(&raw[0..2], &[0x1f, 0x8b]);
        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(raw));
        let mut archive = tar::Archive::new(decoder);
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut data).unwrap();
            entries.push((name, data));
        }
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, "manifest.json");
        assert_eq!(entries[0].1, manifest);
        assert_eq!(entries[1].0, "project.tar");
        assert_eq!(entries[1].1, b"project-dir-tar-bytes");
        assert_eq!(entries[2].0, "volumes/litebin_x_data.tar");
        assert_eq!(entries[2].1, b"volume-tar-bytes");

        std::fs::remove_dir_all(&dir).ok();
    }
}
