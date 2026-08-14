use super::*;

fn report(yaml: &str) -> CompatibilityReport {
    analyze_compose_yaml(yaml, None, None).unwrap().1
}

fn report_for(yaml: &str, project_id: &str) -> CompatibilityReport {
    analyze_compose_yaml(yaml, None, Some(project_id)).unwrap().1
}

fn background_report(yaml: &str) -> CompatibilityReport {
    analyze_compose_yaml_for_workload(yaml, None, None, true).unwrap().1
}

#[test]
fn simple_web_app_is_ok() {
    let r = report(
        r#"
services:
  web:
    image: nginx:alpine
    ports:
      - "8080:80"
"#,
    );
    assert!(r.ok, "findings: {:#?}", r.findings);
    assert!(r.required_capabilities.is_empty());
    assert!(r.findings.iter().any(|f| f.disposition == FindingDisposition::Translated && f.path.contains("ports")));
}

#[test]
fn docker_sock_requires_capability() {
    let r = report(
        r#"
services:
  agent:
    image: example/agent
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
"#,
    );
    assert!(r.ok);
    assert_eq!(r.required_capabilities, vec!["docker-observe".to_string()]);
}

#[test]
fn docker_socket_ancestor_bind_is_unsupported() {
    let r = report(
        r#"
services:
  agent:
    image: example/agent
    volumes:
      - /var:/host-var:ro
"#,
    );
    assert!(!r.ok);
    assert!(r.unsupported().any(|finding| finding.message.contains("contains the Docker daemon socket")));
}

#[test]
fn managed_proxy_service_name_is_reserved() {
    let r = report(
        r#"
services:
  litebin-docker-proxy:
    image: attacker/image
"#,
    );
    assert!(!r.ok);
    assert!(r.unsupported().any(|finding| finding.message.contains("reserved")));
}

#[test]
fn network_mode_host_is_unsupported_for_web_projects() {
    let r = report(
        r#"
services:
  agent:
    image: example/agent
    network_mode: host
"#,
    );
    assert!(!r.ok);
    assert!(r.unsupported().any(|f| f.path.ends_with("network_mode")));
}

#[test]
fn host_observer_requests_only_observation_and_host_network() {
    let r = background_report(
        r#"
services:
  host-agent:
    image: example/host-agent
    network_mode: host
    environment:
      HUB_URL: https://hub.example.com
      LISTEN: "45876"
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      - ./agent_data:/var/lib/host-agent
"#,
    );
    assert!(r.ok, "findings: {:#?}", r.findings);
    assert_eq!(r.required_capabilities, vec!["docker-observe".to_string(), "host-network".to_string()]);
}

#[test]
fn host_mode_rejects_ports_and_custom_networks() {
    let r = background_report(
        r#"
networks:
  custom:
services:
  agent:
    image: example/agent
    network_mode: host
    ports: ["45876:45876"]
    networks: [custom]
"#,
    );
    assert!(!r.ok);
    assert!(r.unsupported().any(|f| f.path == "networks"));
    assert!(r.unsupported().any(|f| f.path.ends_with(".ports")));
    assert!(r.unsupported().any(|f| f.path.ends_with(".networks")));
    assert!(!r.required_capabilities.contains(&"raw-ports".to_string()));
}

#[test]
fn non_public_ports_require_raw_ports() {
    let r = report(
        r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    labels:
      litebin.public: "true"
  db:
    image: postgres
    ports: ["5432:5432"]
"#,
    );
    assert!(r.ok);
    assert!(r.required_capabilities.contains(&"raw-ports".to_string()));
}

#[test]
fn repo_relative_bind_source_is_flagged() {
    let r = report(
        r#"
services:
  web:
    image: nginx
    volumes:
      - ./scripts/init.sh:/init.sh:ro
      - /opt/data:/data
      - pgdata:/var/lib/pg
"#,
    );
    // Relative bind is flagged; absolute host path and named volume are not.
    assert!(r.findings.iter().any(
        |f| f.disposition == FindingDisposition::Translated && f.path.ends_with("(./scripts/init.sh:/init.sh:ro)")
    ));
    assert!(
        !r.findings
            .iter()
            .any(|f| f.disposition == FindingDisposition::Translated && f.path.ends_with("(/opt/data:/data)"))
    );
}

#[test]
fn unknown_service_field_is_unsupported() {
    let r = report(
        r#"
services:
  web:
    image: nginx
    foo_bar: true
"#,
    );
    assert!(!r.ok);
    assert!(r.unsupported().any(|f| f.path.ends_with("foo_bar")));
}

#[test]
fn top_level_networks_overridden() {
    let r = report(
        r#"
networks:
  mynet:
services:
  web:
    image: nginx
"#,
    );
    assert!(r.ok);
    assert!(r.findings.iter().any(|f| { f.path == "networks" && f.disposition == FindingDisposition::Overridden }));
}

#[test]
fn container_name_is_overridden() {
    let r = report(
        r#"
services:
  web:
    image: nginx
    container_name: myweb
"#,
    );
    assert!(r.ok);
    assert!(
        r.findings
            .iter()
            .any(|f| { f.path.ends_with("container_name") && f.disposition == FindingDisposition::Overridden })
    );
}

#[test]
fn findings_use_concrete_names_when_project_id_known() {
    let r = report_for(
        r#"
services:
  host-agent:
    image: example/host-agent
    container_name: agent
    network_mode: host
"#,
        "monitor",
    );
    let container = r.findings.iter().find(|f| f.path.ends_with("container_name")).expect("container_name finding");
    assert!(container.message.contains("litebin-monitor.host-agent"));
    assert!(!container.message.contains('{'));

    let network = r.findings.iter().find(|f| f.path == "litebin.network").expect("network finding");
    assert!(network.message.contains("host namespace"));
    assert!(!network.message.contains('{'));

    let mode = r.unsupported().find(|f| f.path.ends_with("network_mode")).expect("network_mode finding");
    assert!(mode.message.contains("background"));
}
