use anyhow::Result;
use std::path::Path;
use std::process::Command;

use super::gitignored_dirs;

/// Detected project info from `railpack info`.
pub struct ProjectInfo {
    pub project_type: String,
    pub package: String,
}

/// Detect project type by running `railpack info`.
pub fn detect_project(project_dir: &Path) -> Result<ProjectInfo> {
    if project_dir.join("Dockerfile").exists() {
        return Ok(ProjectInfo { project_type: "Dockerfile".to_string(), package: String::new() });
    }

    let project_dir_str = project_dir.to_string_lossy().to_string();

    if cfg!(target_os = "windows") {
        let mut cmd = Command::new("docker");
        cmd.args(["run", "--rm", "-v", &format!("{}:/app", project_dir_str)]);

        // Mask gitignored directories to speed up the mount
        for dir in gitignored_dirs(project_dir) {
            cmd.args(["--tmpfs", &format!("/app/{}", dir)]);
        }

        cmd.args([
            "--entrypoint",
            "sh",
            crate::config::RAILPACK_IMAGE,
            "-c",
            "railpack info /app --format json 2>/dev/null",
        ]);
        cmd.env("MSYS_NO_PATHCONV", "1");

        let output = cmd.output();

        match output {
            Ok(out) if out.status.success() => parse_info_output(&String::from_utf8_lossy(&out.stdout)),
            _ => Ok(ProjectInfo { project_type: "Unknown".to_string(), package: String::new() }),
        }
    } else {
        let bin_path = dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(crate::config::APP_DIR)
            .join("bin")
            .join("railpack");

        if !bin_path.exists() {
            return Ok(ProjectInfo { project_type: "Unknown".to_string(), package: String::new() });
        }

        let output = Command::new(&bin_path).args(["info", &project_dir_str, "--format", "json"]).output();

        match output {
            Ok(out) if out.status.success() => parse_info_output(&String::from_utf8_lossy(&out.stdout)),
            _ => Ok(ProjectInfo { project_type: "Unknown".to_string(), package: String::new() }),
        }
    }
}

fn parse_info_output(json: &str) -> Result<ProjectInfo> {
    let info: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);

    // detectedProviders: e.g. ["staticfile"], ["node"], ["python"]
    let project_type = info["detectedProviders"]
        .as_array()
        .and_then(|arr| {
            let names: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
            if names.is_empty() { None } else { Some(names.join(", ")) }
        })
        .unwrap_or_else(|| "Unknown".to_string());

    // resolvedPackages: e.g. {"caddy": {"name": "caddy", "resolvedVersion": "2.11.2"}}
    let mut packages = Vec::new();
    if let Some(obj) = info["resolvedPackages"].as_object() {
        for (_key, pkg) in obj {
            let name = pkg["name"].as_str().unwrap_or("");
            let version = pkg["resolvedVersion"].as_str().unwrap_or("");
            if !name.is_empty() && !version.is_empty() {
                packages.push(format!("{}@{}", name, version));
            }
        }
    }
    let package = packages.join(", ");

    Ok(ProjectInfo { project_type, package })
}
