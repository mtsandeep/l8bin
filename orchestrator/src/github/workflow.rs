//! The generated workflow, committed into the linked repo. Managed file:
//! auto-deploy is just the `on: push` block being present.

/// Everything the workflow needs; overlaps the project link config.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowSpec {
    pub project_id: String,
    /// Branch the workflow is committed to and deploys from.
    pub branch: String,
    pub auto_deploy: bool,
    pub deploy_kind: DeployKind,
    /// Single mode: internal app port.
    pub port: Option<u16>,
    /// Single mode: optional overrides.
    pub cmd: Option<String>,
    pub dockerfile: Option<String>,
    pub memory_mb: Option<i64>,
    pub cpu: Option<f64>,
    pub node_id: Option<String>,
    /// Subdirectory of the repo the app lives in (None = repo root).
    pub path: Option<String>,
    /// Compose mode: target services (empty = all).
    pub compose_services: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployKind {
    Single,
    Compose,
}

/// Pin by minor tag to avoid migrating generated files.
pub const ACTION_REF: &str = "mtsandeep/l8bin-action@v0.2.0";
pub const WORKFLOW_NAME: &str = "LiteBin Deploy";
/// Bare filename — the form GitHub's dispatch/runs endpoints expect.
pub const WORKFLOW_FILE: &str = "litebin-deploy.yml";
/// Full repo path — where the file is committed (only .github/workflows/ counts).
pub const WORKFLOW_PATH: &str = ".github/workflows/litebin-deploy.yml";

/// YAML-double-quote a scalar.
fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl WorkflowSpec {
    pub fn render(&self) -> String {
        let mut y = String::new();
        y.push_str("# Managed by LiteBin — changes will be overwritten. Edit in the LiteBin dashboard.\n");
        y.push_str(&format!("name: {WORKFLOW_NAME}\n"));
        y.push_str("on:\n");
        y.push_str("  workflow_dispatch: {}\n");
        if self.auto_deploy {
            y.push_str("  push:\n");
            y.push_str(&format!("    branches: [{}]\n", q(&self.branch)));
        }
        y.push_str("\nconcurrency:\n");
        y.push_str(&format!("  group: litebin-{}-${{{{ github.ref }}}}\n", self.project_id));
        y.push_str("  cancel-in-progress: true\n");
        y.push_str("\npermissions:\n");
        y.push_str("  contents: read\n");
        y.push_str("\njobs:\n");
        y.push_str("  deploy:\n");
        y.push_str("    runs-on: ubuntu-latest\n");
        y.push_str("    steps:\n");
        y.push_str("      - uses: actions/checkout@v4\n");
        y.push_str("      - name: Deploy to LiteBin\n");
        y.push_str(&format!("        uses: {ACTION_REF}\n"));
        y.push_str("        with:\n");
        y.push_str("          server: ${{ secrets.L8B_SERVER }}\n");
        y.push_str("          token: ${{ secrets.L8B_TOKEN }}\n");
        y.push_str(&format!("          project_id: {}\n", q(&self.project_id)));
        if let Some(path) = self.path.as_deref().filter(|p| !p.is_empty() && *p != ".") {
            y.push_str(&format!("          path: {}\n", q(path)));
        }
        match self.deploy_kind {
            DeployKind::Single => {
                y.push_str(&format!("          port: '{}'\n", self.port.unwrap_or(3000)));
                if let Some(d) = &self.dockerfile {
                    y.push_str(&format!("          dockerfile: {}\n", q(d)));
                }
                if let Some(c) = &self.cmd {
                    y.push_str(&format!("          cmd: {}\n", q(c)));
                }
            }
            DeployKind::Compose => {
                y.push_str("          compose: 'true'\n");
                if !self.compose_services.is_empty() {
                    let joined = self.compose_services.join(",");
                    y.push_str(&format!("          services: {}\n", q(&joined)));
                }
            }
        }
        if let Some(m) = self.memory_mb {
            y.push_str(&format!("          memory: '{m}'\n"));
        }
        if let Some(c) = self.cpu {
            y.push_str(&format!("          cpu: '{c}'\n"));
        }
        if let Some(n) = &self.node_id {
            y.push_str(&format!("          node_id: {}\n", q(n)));
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> WorkflowSpec {
        WorkflowSpec {
            project_id: "myapp".into(),
            branch: "main".into(),
            auto_deploy: false,
            deploy_kind: DeployKind::Single,
            port: Some(3000),
            cmd: None,
            dockerfile: None,
            memory_mb: Some(512),
            cpu: Some(0.5),
            node_id: None,
            path: None,
            compose_services: vec![],
        }
    }

    #[test]
    fn renders_single_manual_deploy() {
        let y = spec().render();
        assert!(y.contains("workflow_dispatch: {}"));
        assert!(!y.contains("push:"));
        assert!(y.contains("project_id: \"myapp\""));
        assert!(y.contains("port: '3000'"));
        assert!(y.contains("memory: '512'"));
        assert!(y.contains("cpu: '0.5'"));
        assert!(y.contains("group: litebin-myapp-${{ github.ref }}"));
        assert!(y.contains("secrets.L8B_TOKEN"));
    }

    #[test]
    fn renders_auto_deploy_push_trigger() {
        let mut s = spec();
        s.auto_deploy = true;
        let y = s.render();
        assert!(y.contains("push:\n"));
        assert!(y.contains("branches: [\"main\"]"));
    }

    #[test]
    fn renders_compose_mode() {
        let mut s = spec();
        s.deploy_kind = DeployKind::Compose;
        s.compose_services = vec!["web".into(), "api".into()];
        let y = s.render();
        assert!(y.contains("compose: 'true'"));
        assert!(y.contains("services: \"web,api\""));
        assert!(!y.contains("port:"));
    }

    #[test]
    fn renders_subfolder_path() {
        let mut s = spec();
        s.path = Some("apps/web".into());
        let y = s.render();
        assert!(y.contains("path: \"apps/web\""));
        // default spec (no path) omits the input entirely
        assert!(!spec().render().contains("path:"));
    }

    #[test]
    fn quotes_yaml_specials_in_project_id() {
        let mut s = spec();
        s.project_id = "we\"ird".into();
        let y = s.render();
        assert!(y.contains("project_id: \"we\\\"ird\""));
    }
}
