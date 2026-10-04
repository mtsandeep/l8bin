//! `l8b mcp` — stdio MCP server exposing the CLI as tools.
//!
//! Each tool runs `l8b <command> --json` as a subprocess and returns its
//! stdout as the tool result. This guarantees the protocol channel stays
//! clean (no stray progress output), inherits the CLI's auth (token/session
//! config or env), and honors `l8b.toml` defaults from the working directory.
//!
//! Transport: newline-delimited JSON-RPC 2.0 over stdio (MCP stdio).

use anyhow::Result;
use serde_json::{Value, json};

struct Tool {
    name: &'static str,
    description: &'static str,
    schema: Value,
    /// Map tools/call arguments to `l8b` argv + optional stdin.
    invoke: fn(args: &Value) -> (Vec<String>, Option<String>),
}

/// Non-capturing closure per verb — coerces to the fn-pointer field type.
macro_rules! verb {
    ($name:expr) => {
        |a: &Value| {
            let mut argv = vec![$name.to_string(), "--json".into()];
            if let Some(v) = a["project"].as_str() {
                argv.push(v.into());
            }
            (argv, None)
        }
    };
}

fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "deploy",
            description: "Build and deploy a project directory to LiteBin (Dockerfile auto-detected, Railpack fallback). Polls until running and returns status + URL.",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "Project ID (default: l8b.toml)"},
                    "server": {"type": "string", "description": "LiteBin server URL (default: l8b.toml server, else your only login; refused with the choices if ambiguous)"},
                    "path": {"type": "string", "description": "Project directory (default: cwd)"},
                    "port": {"type": "integer", "description": "Internal app port"},
                    "env_file": {"type": "string", "description": "Push this file as runtime env after deploying"},
                    "background": {"type": "boolean", "description": "Background project (no HTTP URL)"}
                }
            }),
            invoke: |a| {
                let mut argv = vec!["deploy".into(), "--json".into()];
                if let Some(v) = a["server"].as_str() {
                    argv.push(format!("--server={v}"));
                }
                if let Some(v) = a["project"].as_str() {
                    argv.push(format!("--project={v}"));
                }
                if let Some(v) = a["path"].as_str() {
                    argv.push(format!("--path={v}"));
                }
                if let Some(v) = a["port"].as_u64() {
                    argv.push(format!("--port={v}"));
                }
                if let Some(v) = a["env_file"].as_str() {
                    argv.push(format!("--env-file={v}"));
                }
                if a["background"].as_bool() == Some(true) {
                    argv.push("--background".into());
                }
                (argv, None)
            },
        },
        Tool {
            name: "status",
            description: "Project status. With wait=true, polls to a terminal state (non-zero when not running). With healthy=true, also probes the live URL for a 2xx — the definitive 'is it up?' answer.",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "Project ID (default: l8b.toml)"},
                    "server": {"type": "string", "description": "LiteBin server URL (default: l8b.toml server, else your only login; refused with the choices if ambiguous)"},
                    "wait": {"type": "boolean", "description": "Wait for a terminal state"},
                    "healthy": {"type": "boolean", "description": "Probe the URL for a 2xx (implies wait)"},
                    "timeout": {"type": "integer", "description": "Wait timeout seconds (default 120)"}
                }
            }),
            invoke: |a| {
                let mut argv = vec!["status".into(), "--json".into()];
                if let Some(v) = a["server"].as_str() {
                    argv.push(format!("--server={v}"));
                }
                if let Some(v) = a["project"].as_str() {
                    argv.push(format!("--project={v}"));
                }
                if a["wait"].as_bool() == Some(true) || a["healthy"].as_bool() == Some(true) {
                    argv.push("--wait".into());
                }
                if a["healthy"].as_bool() == Some(true) {
                    argv.push("--healthy".into());
                }
                if let Some(v) = a["timeout"].as_u64() {
                    argv.push(format!("--timeout={v}"));
                }
                (argv, None)
            },
        },
        Tool {
            name: "list",
            description: "List all projects with live status (running first) and URLs.",
            schema: json!({"type": "object", "properties": {}}),
            invoke: |_| (vec!["list".into(), "--json".into()], None),
        },
        Tool {
            name: "logs",
            description: "Container logs for a project (or deploy logs with deploy=true).",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "Project ID (default: l8b.toml)"},
                    "server": {"type": "string", "description": "LiteBin server URL (default: l8b.toml server, else your only login; refused with the choices if ambiguous)"},
                    "tail": {"type": "integer", "description": "Number of lines (default 100)"},
                    "service": {"type": "string", "description": "Service name for multi-service projects"},
                    "deploy": {"type": "boolean", "description": "Show deploy logs instead"}
                }
            }),
            invoke: |a| {
                let project = a["project"].as_str().map(str::to_string);
                let mut argv = vec!["logs".into(), "--json".into()];
                if let Some(v) = a["server"].as_str() {
                    argv.push(format!("--server={v}"));
                }
                if let Some(v) = a["tail"].as_u64() {
                    argv.push(format!("--tail={v}"));
                }
                if let Some(v) = a["service"].as_str() {
                    argv.push(format!("--service={v}"));
                }
                if a["deploy"].as_bool() == Some(true) {
                    argv.push("--deploy".into());
                }
                if let Some(p) = project {
                    argv.push(p);
                }
                (argv, None)
            },
        },
        Tool {
            name: "env_list",
            description: "Runtime env keys with masked previews (values are write-only).",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "Project ID (default: l8b.toml)"},
                    "server": {"type": "string", "description": "LiteBin server URL (default: l8b.toml server, else your only login; refused with the choices if ambiguous)"}
                }
            }),
            invoke: |a| {
                let mut argv = vec!["env".into(), "list".into(), "--json".into()];
                if let Some(v) = a["server"].as_str() {
                    argv.push(format!("--server={v}"));
                }
                if let Some(v) = a["project"].as_str() {
                    argv.push(v.into());
                }
                (argv, None)
            },
        },
        Tool {
            name: "env_push",
            description: "Set runtime env variables (KEY→value map). merge by default; replace removes unlisted keys. apply=true recreates the container to apply immediately.",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "Project ID (default: l8b.toml)"},
                    "server": {"type": "string", "description": "LiteBin server URL (default: l8b.toml server, else your only login; refused with the choices if ambiguous)"},
                    "env": {"type": "object", "description": "KEY → value pairs to set", "additionalProperties": {"type": "string"}},
                    "replace": {"type": "boolean", "description": "Remove keys not present in env"},
                    "apply": {"type": "boolean", "description": "Recreate to apply now"}
                },
                "required": ["env"]
            }),
            invoke: |a| {
                let mut argv = vec!["env".into(), "push".into(), "--stdin".into(), "--json".into()];
                if let Some(v) = a["server"].as_str() {
                    argv.push(format!("--server={v}"));
                }
                if a["replace"].as_bool() == Some(true) {
                    argv.push("--replace".into());
                }
                if a["apply"].as_bool() == Some(true) {
                    argv.push("--apply".into());
                }
                if let Some(v) = a["project"].as_str() {
                    argv.push(v.into());
                }
                let mut stdin = String::new();
                if let Some(map) = a["env"].as_object() {
                    for (k, v) in map {
                        if let Some(val) = v.as_str() {
                            stdin.push_str(&format!("{k}={val}\n"));
                        }
                    }
                }
                (argv, Some(stdin))
            },
        },
        Tool {
            name: "stop",
            description: "Stop a running project (idempotent).",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string", "description": "Project ID (default: l8b.toml)"}}
            }),
            invoke: verb!("stop"),
        },
        Tool {
            name: "start",
            description: "Start a stopped project; polls until running.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string", "description": "Project ID (default: l8b.toml)"}}
            }),
            invoke: verb!("start"),
        },
        Tool {
            name: "restart",
            description: "Recreate the project's containers (applies pending .env changes); polls until running.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string", "description": "Project ID (default: l8b.toml)"}}
            }),
            invoke: verb!("restart"),
        },
        Tool {
            name: "url",
            description: "The project's managed URL (empty for background projects).",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string", "description": "Project ID (default: l8b.toml)"}}
            }),
            invoke: |a| {
                let mut argv = vec!["url".into(), "--json".into()];
                if let Some(v) = a["project"].as_str() {
                    argv.push(v.into());
                }
                (argv, None)
            },
        },
        Tool {
            name: "domain_set",
            description: "Set a project's custom domain (LiteBin provisions TLS).",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "Project ID (default: l8b.toml)"},
                    "domain": {"type": "string", "description": "e.g. myapp.example.com"}
                },
                "required": ["domain"]
            }),
            invoke: |a| {
                let mut argv = vec!["domain".into(), "set".into(), "--json".into()];
                if let Some(v) = a["project"].as_str() {
                    argv.push(format!("--project={v}"));
                }
                if let Some(v) = a["domain"].as_str() {
                    argv.push(v.into());
                }
                (argv, None)
            },
        },
        Tool {
            name: "domain_remove",
            description: "Clear a project's custom domain.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string", "description": "Project ID (default: l8b.toml)"}}
            }),
            invoke: |a| {
                let mut argv = vec!["domain".into(), "remove".into(), "--json".into()];
                if let Some(v) = a["project"].as_str() {
                    argv.push(format!("--project={v}"));
                }
                (argv, None)
            },
        },
        Tool {
            name: "delete",
            description: "Delete a project with its containers and volumes (admin scope). Requires confirm=true.",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "Project ID (default: l8b.toml)"},
                    "confirm": {"type": "boolean", "description": "Must be true"}
                },
                "required": ["confirm"]
            }),
            invoke: |a| {
                if a["confirm"].as_bool() != Some(true) {
                    return (vec!["__rejected".into()], None);
                }
                let mut argv = vec!["delete".into(), "--yes".into(), "--json".into()];
                if let Some(v) = a["project"].as_str() {
                    argv.push(v.into());
                }
                (argv, None)
            },
        },
        Tool {
            name: "doctor",
            description: "Environment sanity checks (server reachable, auth valid, docker present, l8b.toml found) with recovery hints.",
            schema: json!({"type": "object", "properties": {}}),
            invoke: |_| (vec!["doctor".into(), "--json".into()], None),
        },
    ]
}

pub async fn run() -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut line = String::new();

    loop {
        line.clear();
        if std::io::BufRead::read_line(&mut reader, &mut line)? == 0 {
            return Ok(()); // EOF — client closed
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(m) => m,
            Err(_) => continue, // ignore malformed lines
        };
        if msg.get("jsonrpc").is_none() {
            continue;
        }
        let response = handle(msg).await;
        if let Some(r) = response {
            println!("{}", serde_json::to_string(&r)?);
        }
    }
}

async fn handle(msg: Value) -> Option<Value> {
    // notifications get no response
    msg.get("id")?;
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "litebin", "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": tools().iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.schema,
            })).collect::<Vec<_>>()
        })),
        "tools/call" => tools_call(&params).await,
        _ => Err(format!("method not found: {method}")),
    };

    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": e}}),
    })
}

async fn tools_call(params: &Value) -> Result<Value, String> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let all = tools();
    let Some(tool) = all.iter().find(|t| t.name == name) else {
        return Err(format!("unknown tool: {name}"));
    };

    let (argv, stdin_data) = (tool.invoke)(&args);
    if argv.first().map(String::as_str) == Some("__rejected") {
        return Ok(json!({
            "content": [{"type": "text", "text": "Refused: confirm must be true to delete a project."}],
            "isError": true
        }));
    }

    let exe = std::env::current_exe().map_err(|e| format!("failed to resolve l8b binary: {e}"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&argv)
        .stdin(if stdin_data.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // inherit the auth environment as-is (L8B_SERVER/L8B_TOKEN or stored config)

    let mut spawned = cmd.spawn().map_err(|e| format!("failed to spawn l8b: {e}"))?;
    if let Some(data) = &stdin_data
        && let Some(mut si) = spawned.stdin.take()
    {
        use std::io::Write;
        si.write_all(data.as_bytes()).map_err(|e| format!("failed to feed stdin: {e}"))?;
    }
    let output = spawned.wait_with_output().map_err(|e| format!("tool failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let ok = output.status.success();
    let text = if ok { stdout } else { format!("{stdout}\n{stderr}").trim().to_string() };

    Ok(json!({
        "content": [{"type": "text", "text": text}],
        "isError": !ok
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schemas_are_valid_json_objects() {
        for t in tools() {
            assert_eq!(t.schema["type"], "object", "{}", t.name);
        }
    }

    #[test]
    fn env_push_maps_object_to_stdin() {
        let all = tools();
        let tool = all.iter().find(|t| t.name == "env_push").unwrap();
        let (_, stdin) = (tool.invoke)(&json!({"env": {"A": "1", "B": "two"}, "apply": true}));
        let stdin = stdin.unwrap();
        assert!(stdin.contains("A=1\n") && stdin.contains("B=two\n"));
    }

    #[test]
    fn delete_requires_confirm() {
        let all = tools();
        let tool = all.iter().find(|t| t.name == "delete").unwrap();
        let (argv, _) = (tool.invoke)(&json!({"confirm": false}));
        assert_eq!(argv[0], "__rejected");
    }
}
