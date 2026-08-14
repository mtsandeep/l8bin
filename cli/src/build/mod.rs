mod context;
mod detect;
mod dockerbuild;
mod railpack;

use std::path::Path;

use anyhow::Result;

pub(crate) use context::gitignored_dirs;
pub use context::{BuildContextGuard, cleanup_build_artifacts};
pub use detect::{ProjectInfo, detect_project};

use dockerbuild::build_with_docker;
use railpack::{build_with_railpack_docker, build_with_railpack_native};

/// Result of building a Docker image.
pub struct SavedImage {
    pub path: String,
    pub image_id: String,
    pub image_size: u64,
    pub compressed_size: u64,
}

/// Detect build strategy and produce a Docker image tar file.
/// When `ci_mode` is true, build logs are suppressed to prevent secret leakage.
pub async fn build_project(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    secret: Vec<std::path::PathBuf>,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    build_project_inner(project_dir, dockerfile, image_tag, secret, true, ci_mode, platform).await
}

async fn build_project_inner(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    secret: Vec<std::path::PathBuf>,
    quiet: bool,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    let has_dockerfile =
        if let Some(df) = dockerfile { project_dir.join(df).exists() } else { project_dir.join("Dockerfile").exists() };

    // Context guard manages .dockerignore always, and .env injection when secrets are provided
    let _ctx_guard = BuildContextGuard::new(project_dir, dockerfile.unwrap_or("Dockerfile"), secret)?;

    let result = if has_dockerfile {
        build_with_docker(project_dir, dockerfile, image_tag, Some(&_ctx_guard), quiet, ci_mode, platform).await
    } else if cfg!(target_os = "windows") {
        dockerbuild::check_docker_available()?;
        build_with_railpack_docker(project_dir, image_tag, Some(&_ctx_guard), quiet, ci_mode, platform).await
    } else {
        build_with_railpack_native(project_dir, image_tag, Some(&_ctx_guard), quiet, ci_mode, platform).await
    };

    // Temp .dockerignore and .env are cleaned up when _ctx_guard drops
    result
}
