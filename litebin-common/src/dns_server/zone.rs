//! Split-horizon zone specification: which names answer with the host gateway
//! instead of being forwarded upstream, plus persistence across restarts.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn default_ttl() -> u32 {
    5
}

/// Internal DNS view pushed by the orchestrator (or agent relay).
///
/// A query name matches when it equals an `exact_names` entry, or when it is
/// a `wildcard_domains` entry or lies beneath one (`litebin.dev` covers
/// `app.litebin.dev` but not `evillitebin.dev`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ZoneSpec {
    /// Gateway IP answered (A) for every matching name.
    pub answer_ip: String,
    #[serde(default = "default_ttl")]
    pub ttl_secs: u32,
    /// Suffix zones (e.g. the platform domain during its grace period).
    pub wildcard_domains: Vec<String>,
    /// Exact names (custom domains and their www pairs, the apex).
    pub exact_names: Vec<String>,
}

impl ZoneSpec {
    pub fn matches(&self, qname: &str) -> bool {
        let q = normalize(qname);
        if q.is_empty() {
            return false;
        }
        if self.exact_names.iter().any(|n| normalize(n) == q) {
            return true;
        }
        self.wildcard_domains.iter().any(|d| {
            let d = normalize(d);
            q == d || q.strip_suffix(&format!(".{d}")).is_some()
        })
    }

    /// Stable digest of the canonical JSON — used for push dedupe (204) and
    /// health reporting.
    pub fn hash_hex(&self) -> String {
        let canonical = serde_json::to_vec(self).unwrap_or_default();
        let digest = Sha256::digest(&canonical);
        hex::encode(digest)
    }

    pub fn load(path: &std::path::Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

fn normalize(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone() -> ZoneSpec {
        ZoneSpec {
            answer_ip: "172.17.0.1".into(),
            ttl_secs: 5,
            wildcard_domains: vec!["litebin.dev".into(), "old.dev".into()],
            exact_names: vec!["app.example.com".into(), "www.app.example.com".into()],
        }
    }

    #[test]
    fn wildcard_matches_name_and_subdomains_not_suffix_lookalikes() {
        let z = zone();
        assert!(z.matches("litebin.dev"));
        assert!(z.matches("App.LiteBin.Dev."));
        assert!(z.matches("proj.litebin.dev"));
        assert!(z.matches("a.b.litebin.dev"));
        assert!(!z.matches("evillitebin.dev"));
        assert!(!z.matches("litebin.dev.evil.com"));
    }

    #[test]
    fn exact_names_match_and_others_dont() {
        let z = zone();
        assert!(z.matches("app.example.com"));
        assert!(z.matches("WWW.APP.EXAMPLE.COM"));
        assert!(!z.matches("sub.app.example.com")); // exact, not wildcard
        assert!(!z.matches("example.com"));
        assert!(!z.matches(""));
    }

    #[test]
    fn previous_domain_grace_is_wildcarded() {
        let z = zone();
        assert!(z.matches("dash.old.dev"));
        assert!(z.matches("proj.old.dev"));
    }

    #[test]
    fn hash_changes_with_content_and_roundtrips() {
        let z = zone();
        let mut dir = std::env::temp_dir();
        dir.push(format!("l8b-dns-zone-{}.json", std::process::id()));
        z.save(&dir).unwrap();
        let loaded = ZoneSpec::load(&dir).unwrap();
        assert_eq!(loaded, z);
        assert_eq!(loaded.hash_hex(), z.hash_hex());
        let _ = std::fs::remove_file(&dir);

        let mut other = z.clone();
        other.answer_ip = "172.18.0.1".into();
        assert_ne!(other.hash_hex(), z.hash_hex());
    }
}
