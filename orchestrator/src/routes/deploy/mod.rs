pub(crate) mod compose;
pub mod logs;
pub mod single;

/// Resolved public URL for deploy responses: custom domain when set, else the
/// managed subdomain. Responses also carry the raw `custom_domain` so clients
/// can pick the other one.
pub(crate) fn resolved_project_url(project_id: &str, custom_domain: Option<&str>, domain: &str) -> String {
    match custom_domain.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => format!("https://{d}"),
        None => format!("https://{project_id}.{domain}"),
    }
}

#[cfg(test)]
mod tests {
    use super::resolved_project_url;

    #[test]
    fn prefers_custom_domain() {
        assert_eq!(
            resolved_project_url("app", Some(" example.com "), "l8b.in"),
            "https://example.com"
        );
    }

    #[test]
    fn empty_custom_domain_falls_back_to_subdomain() {
        assert_eq!(resolved_project_url("app", Some("  "), "l8b.in"), "https://app.l8b.in");
        assert_eq!(resolved_project_url("app", None, "l8b.in"), "https://app.l8b.in");
    }
}
