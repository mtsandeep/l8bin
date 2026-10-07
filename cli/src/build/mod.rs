mod context;
mod detect;
mod dockerbuild;
mod railpack;
mod secrets;

use std::path::Path;

use anyhow::Result;

pub use context::cleanup_build_artifacts;
pub(crate) use context::gitignored_dirs;
pub use detect::{ProjectInfo, detect_project};

use context::BuildContextGuard;
use dockerbuild::build_with_docker;
use railpack::{build_with_railpack_docker, build_with_railpack_native};

/// Result of building a Docker image.
pub struct SavedImage {
    pub path: String,
    pub image_id: String,
    pub image_size: u64,
    pub compressed_size: u64,
}

/// Build-time env selection for one image build.
pub struct BuildEnv {
    /// .env* files inside the build context dir. Dockerfile builds re-include
    /// them via the generated ignore file's negations; Railpack builds parse them for secrets.
    pub context_files: Vec<std::path::PathBuf>,
    /// Env files outside the context (multi-service root .env files). Consumed
    /// only by Railpack builds — a Docker build context cannot include them.
    pub external_files: Vec<std::path::PathBuf>,
}

/// Detect build strategy and produce a Docker image tar file.
/// When `ci_mode` is true, build logs are suppressed to prevent secret leakage.
pub async fn build_project(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    env: BuildEnv,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    build_project_inner(project_dir, dockerfile, image_tag, env, true, ci_mode, platform).await
}

async fn build_project_inner(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    env: BuildEnv,
    quiet: bool,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    let has_dockerfile =
        if let Some(df) = dockerfile { project_dir.join(df).exists() } else { project_dir.join("Dockerfile").exists() };

    if has_dockerfile {
        // Generated ignore file makes selected env files part of the context; cleaned up on drop
        let _ctx_guard =
            BuildContextGuard::new(project_dir, dockerfile.unwrap_or("Dockerfile"), &env.context_files, ci_mode)?;
        build_with_docker(project_dir, dockerfile, image_tag, quiet, ci_mode, platform).await
    } else {
        // Railpack builds take env as native build secrets — no context files needed
        let (env_pairs, skipped) = secrets::railpack_env_pairs(&env.external_files, &env.context_files)?;
        secrets::warn_skipped(&skipped, ci_mode);

        if cfg!(target_os = "windows") {
            dockerbuild::check_docker_available()?;
            build_with_railpack_docker(project_dir, image_tag, &env_pairs, quiet, ci_mode, platform).await
        } else {
            build_with_railpack_native(project_dir, image_tag, &env_pairs, quiet, ci_mode, platform).await
        }
    }
}
