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
    id.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
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
}
