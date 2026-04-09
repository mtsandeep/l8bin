/// CI mode — suppresses verbose output and prevents secret leakage in logs.
pub struct CiMode {
    pub enabled: bool,
    /// True when running inside GitHub Actions (detected via GITHUB_ACTIONS env).
    pub github_actions: bool,
}

impl CiMode {
    /// Resolve CI mode from explicit flag, env var, or environment detection.
    /// Priority: --ci flag > L8B_CI env > GITHUB_ACTIONS auto-detect
    pub fn from_flag(ci_flag: bool) -> Self {
        let github_actions =
            std::env::var("GITHUB_ACTIONS").map(|v| v == "true").unwrap_or(false);
        let enabled = ci_flag
            || std::env::var("L8B_CI").map(|v| v == "true").unwrap_or(false)
            || github_actions;
        Self {
            enabled,
            github_actions,
        }
    }

    /// Register a secret value with GitHub Actions log masking.
    pub fn mask_secret(&self, value: &str) {
        if self.github_actions && !value.is_empty() {
            println!("::add-mask::{}", value);
        }
    }

    /// Print a line only when NOT in CI mode.
    pub fn println(&self, msg: &str) {
        if !self.enabled {
            println!("{}", msg);
        }
    }
}
