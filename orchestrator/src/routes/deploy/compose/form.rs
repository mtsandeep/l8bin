/// Parsed multipart form fields for `/deploy/compose`.
pub(super) struct ComposeForm {
    pub project_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub node_id: Option<String>,
    pub auto_stop_enabled: Option<bool>,
    pub auto_stop_timeout_mins: Option<i64>,
    pub auto_start_enabled: Option<bool>,
    pub is_background: Option<bool>,
    pub custom_domain: Option<String>,
    pub allow_raw_ports: Option<bool>,
    pub grant_capabilities_raw: Option<String>,
    pub compose_yaml: String,
    pub target_services_raw: Option<String>,
    pub stage_only_requested: bool,
}
