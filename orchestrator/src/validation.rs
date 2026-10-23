/// Check if a project ID is a valid DNS label.
/// - Lowercase alphanumeric and hyphens only
/// - 1-63 characters
/// - No leading or trailing hyphens
pub fn is_valid_project_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 63 {
        return false;
    }
    if id.starts_with('-') || id.ends_with('-') {
        return false;
    }
    id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Check if a sqlx error is a SQLite UNIQUE constraint violation (error code 2067).
pub fn is_unique_constraint(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("2067"))
}

/// Strict custom-domain validation shared by deploy and settings paths.
/// Returns the canonical form (trimmed, lowercased, trailing dot stripped).
/// `www.` is kept as entered — www vs bare decides which host is canonical
/// (the other variant redirects to it).
pub fn validate_custom_domain(raw: &str) -> Result<String, String> {
    let d = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() {
        return Err("domain must not be empty".into());
    }
    if d.contains('*') {
        return Err("wildcard domains are not allowed".into());
    }
    if d.len() > 253 {
        return Err("domain too long (max 253 characters)".into());
    }
    if !d.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return Err("domain must be a plain hostname (letters, digits, hyphens, dots)".into());
    }
    if d != "localhost" {
        let labels: Vec<&str> = d.split('.').collect();
        if labels.len() < 2 {
            return Err("domain must include a TLD (e.g. app.example.com) or be localhost".into());
        }
        if labels.iter().any(|l| l.is_empty() || l.len() > 63 || l.starts_with('-') || l.ends_with('-')) {
            return Err("invalid domain label (empty, too long, or leading/trailing hyphen)".into());
        }
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ids() {
        assert!(is_valid_project_id("my-app"));
        assert!(is_valid_project_id("app123"));
        assert!(is_valid_project_id("a"));
        assert!(is_valid_project_id("my-cool-app-v2"));
        assert!(is_valid_project_id(&"a".repeat(63)));
    }

    #[test]
    fn invalid_ids() {
        assert!(!is_valid_project_id(""));
        assert!(!is_valid_project_id(&"a".repeat(64)));
        assert!(!is_valid_project_id("-leading"));
        assert!(!is_valid_project_id("trailing-"));
        assert!(!is_valid_project_id("MY-APP"));
        assert!(!is_valid_project_id("my_app"));
        assert!(!is_valid_project_id("my.app"));
        assert!(!is_valid_project_id("my app"));
    }

    #[test]
    fn valid_custom_domains() {
        assert_eq!(validate_custom_domain("app.example.com").unwrap(), "app.example.com");
        assert_eq!(validate_custom_domain("  WWW.Example.COM.  ").unwrap(), "www.example.com");
        assert_eq!(validate_custom_domain("localhost").unwrap(), "localhost");
        assert_eq!(validate_custom_domain("a-b.c-d.example.co").unwrap(), "a-b.c-d.example.co");
    }

    #[test]
    fn invalid_custom_domains() {
        assert!(validate_custom_domain("*").is_err());
        assert!(validate_custom_domain("*.example.com").is_err());
        assert!(validate_custom_domain("app.*.com").is_err());
        assert!(validate_custom_domain("https://example.com").is_err());
        assert!(validate_custom_domain("app example.com").is_err());
        assert!(validate_custom_domain("app_example.com").is_err());
        assert!(validate_custom_domain(".app.example.com").is_err());
        assert!(validate_custom_domain("app..example.com").is_err());
        assert!(validate_custom_domain("-app.example.com").is_err());
        assert!(validate_custom_domain("app").is_err());
        assert!(validate_custom_domain("").is_err());
    }
}
