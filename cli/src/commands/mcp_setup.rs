//! `l8b __setup` — the state machine behind the `setup` MCP tool. Agents
//! drive it: status (no args) → pair (`--server`) → complete (`--complete`)
//! → bind (`--action=bind`). The MCP always invokes it with `--json`, so
//! every response is one machine-readable object with a `next` instruction.

use anyhow::{Result, bail};
use clap::ValueEnum;
use serde_json::json;
use std::path::Path;

use crate::auth::{self, PairingPoll};
use crate::config::{CredentialStore, PairingSession, ServerCredential, normalize_server, unix_now};
use crate::out::Out;

/// Cap on one `complete` call's polling — under the server's device-token
/// rate limit and typical MCP tool timeouts.
const COMPLETE_POLL_SECS: i64 = 60;

pub(crate) struct SetupToolArgs {
    pub server: Option<String>,
    pub complete: bool,
    pub action: Option<SetupAction>,
    pub scope: Option<String>,
}

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SetupAction {
    Status,
    Bind,
}

pub(crate) async fn run(args: SetupToolArgs, out: &Out) -> Result<()> {
    let server = args.server.as_deref().map(normalize_server);
    match (server, args.action.unwrap_or(SetupAction::Status)) {
        (Some(s), SetupAction::Bind) => bind(&s, out).await,
        (None, SetupAction::Bind) => {
            let logins =
                CredentialStore::load().logged_in_servers().iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
            bail!(crate::out::fail(
                "bind needs a server",
                format!(
                    "pass one of the stored logins{hint}",
                    hint = if logins.is_empty() { String::new() } else { format!(": {logins}") }
                ),
            ));
        }
        (server, SetupAction::Status) if args.complete => {
            let server = server.or_else(|| PairingSession::load().map(|p| p.server));
            match server {
                Some(s) => complete(&s, out).await,
                None => bail!(crate::out::fail(
                    "no pairing in progress",
                    "start one: call setup with {\"server\": \"<url>\"}"
                )),
            }
        }
        (None, SetupAction::Status) => status(out),
        (Some(s), SetupAction::Status) => {
            // Smart path: pair only when needed.
            if CredentialStore::load().get(&s).is_some() {
                bind(&s, out).await
            } else {
                start(&s, args.scope.as_deref(), out).await
            }
        }
    }
}

/// Pure snapshot of where this machine/workspace stands.
#[derive(Debug, PartialEq)]
enum SetupState {
    /// l8b.toml names a server that has a stored login.
    Configured { server: String },
    /// l8b.toml names a server with no stored login.
    Misconfigured { server: String },
    /// An unexpired pairing session exists.
    PairingPending { server: String },
    /// No l8b.toml server; exactly one login.
    NeedsBinding { server: String },
    /// No l8b.toml server; multiple logins.
    Ambiguous { choices: Vec<String> },
    /// No logins, no binding, no pairing.
    Unconfigured,
}

fn compute_state(toml_server: Option<&str>, logins: &[String], pairing: Option<&PairingSession>) -> SetupState {
    if let Some(s) = toml_server {
        let s = normalize_server(s);
        return if logins.contains(&s) {
            SetupState::Configured { server: s }
        } else {
            SetupState::Misconfigured { server: s }
        };
    }
    if let Some(p) = pairing.filter(|p| !p.expired()) {
        return SetupState::PairingPending { server: p.server.clone() };
    }
    match logins.len() {
        0 => SetupState::Unconfigured,
        1 => SetupState::NeedsBinding { server: logins[0].clone() },
        _ => SetupState::Ambiguous { choices: logins.to_vec() },
    }
}

fn state_name(state: &SetupState) -> &'static str {
    match state {
        SetupState::Configured { .. } => "configured",
        SetupState::Misconfigured { .. } => "misconfigured",
        SetupState::PairingPending { .. } => "pairing_pending",
        SetupState::NeedsBinding { .. } => "needs_binding",
        SetupState::Ambiguous { .. } => "ambiguous",
        SetupState::Unconfigured => "unconfigured",
    }
}

/// The exact follow-up for the driving agent, embedded with the tool call to make.
fn next_instruction(state: &SetupState) -> String {
    match state {
        SetupState::Configured { .. } =>
            "Nothing to do — auth is stored and this workspace is bound. Verify live access with the `doctor` tool, then `deploy`.".into(),
        SetupState::Misconfigured { server } =>
            format!("This workspace's l8b.toml names {server} but there is no stored login for it. If that is still the right server, call setup with {{\"server\": \"{server}\"}} to pair; otherwise bind a different one with {{\"server\": \"<url>\", \"action\": \"bind\"}}."),
        SetupState::PairingPending { server } =>
            format!("A pairing is waiting for approval. Have the user open the approval_url from the earlier response in their browser, then call setup with {{\"server\": \"{server}\", \"complete\": true}}."),
        SetupState::NeedsBinding { server } =>
            format!("One stored login ({server}) is not yet bound to this workspace. If that is the intended server, call setup with {{\"server\": \"{server}\", \"action\": \"bind\"}}; otherwise ask the user which server this project deploys to."),
        SetupState::Ambiguous { choices } =>
            format!("Multiple stored logins and this workspace is not bound to one. Ask the user which server this project deploys to, then call setup with {{\"server\": \"<url>\", \"action\": \"bind\"}}. Choices: {}", choices.join(", ")),
        SetupState::Unconfigured =>
            "No LiteBin auth on this machine. Ask the user for their LiteBin server URL (or self-host: https://l8bin.com), then call setup with {\"server\": \"<url>\"}.".into(),
    }
}

fn status(out: &Out) -> Result<()> {
    let store = CredentialStore::load();
    let logins: Vec<String> = store.logged_in_servers().iter().map(|s| s.to_string()).collect();
    let pairing = PairingSession::load();
    let toml_server = crate::project_config::load(Path::new(".")).and_then(|c| c.server);
    let state = compute_state(toml_server.as_deref(), &logins, pairing.as_ref());

    let mut payload = json!({
        "state": state_name(&state),
        "logins": logins,
        "workspace_dir": std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default(),
        "next": next_instruction(&state),
    });
    match &state {
        SetupState::Configured { server } | SetupState::Misconfigured { server } => {
            payload["server"] = json!(server);
        }
        SetupState::PairingPending { .. } => {
            let p = pairing.as_ref().unwrap();
            payload["pairing"] = json!({
                "server": p.server,
                "user_code": p.user_code,
                "approval_url": p.approval_url(),
                "expires_at": p.expires_at,
            });
        }
        _ => {}
    }
    emit(out, &payload)
}

/// Start device pairing for `server` and persist the session.
async fn start(server: &str, scope: Option<&str>, out: &Out) -> Result<()> {
    // Fail fast on an unreachable URL instead of a pairing that can never be approved.
    let probe = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build()?;
    match probe.get(format!("{}/health", server.trim_end_matches('/'))).send().await {
        Ok(r) if r.status().is_success() => {}
        Ok(r) => bail!(crate::out::fail(
            format!("server unhealthy at {server} (HTTP {})", r.status()),
            "verify the URL — is LiteBin running there? Self-host one: https://l8bin.com",
        )),
        Err(e) => bail!(crate::out::fail(
            format!("server unreachable at {server}: {e}"),
            "verify the URL — is LiteBin running there? Self-host one: https://l8bin.com",
        )),
    }

    let client_name = std::env::var("L8B_CLIENT_NAME").unwrap_or_else(|_| "l8b MCP".to_string());
    let start = auth::start_pairing(server, scope.unwrap_or("manage"), &client_name).await?;
    let session = PairingSession {
        server: start.server.clone(),
        device_code: start.device_code,
        user_code: start.user_code,
        interval: start.interval,
        expires_at: start.expires_at,
    };
    session.save()?; // last start wins — concurrent setups clobber, fine for one machine

    emit(
        out,
        &json!({
            "state": "pairing_pending",
            "server": session.server,
            "approval_url": session.approval_url(),
            "user_code": session.user_code,
            "expires_in": (session.expires_at - unix_now()).max(0),
            "interval": session.interval,
            "next": format!("Have the user open approval_url in a browser where they are logged into the LiteBin dashboard; they approve and pick the token scope. Then call setup with {{\"server\": \"{}\", \"complete\": true}}.", session.server),
        }),
    )
}

/// Poll the pending pairing; on approval store the credential and bind the workspace.
async fn complete(server: &str, out: &Out) -> Result<()> {
    if CredentialStore::load().get(server).is_some() {
        return bind(server, out).await; // a later CLI login already satisfied this server
    }
    let Some(session) = PairingSession::load() else {
        bail!(crate::out::fail(
            format!("no pairing in progress for {server}"),
            format!("start one: call setup with {{\"server\": \"{server}\"}}"),
        ));
    };
    if session.expired() {
        PairingSession::clear();
        bail!(crate::out::fail(
            "pairing code expired",
            format!("start a new one: call setup with {{\"server\": \"{server}\"}}"),
        ));
    }
    if session.server != server {
        bail!(crate::out::fail(
            format!("the pending pairing is for {}, not {server}", session.server),
            format!(
                "call setup with {{\"server\": \"{}\", \"complete\": true}} to finish it, or start a new one for {server}",
                session.server
            ),
        ));
    }

    let client = reqwest::Client::new();
    let deadline = session.expires_at.min(unix_now() + COMPLETE_POLL_SECS);
    loop {
        match auth::poll_pairing(&client, server, &session.device_code).await {
            PairingPoll::Approved { token, scope, project_id } => {
                let mut store = CredentialStore::load();
                store.upsert(server, ServerCredential { token: Some(token), scope: Some(scope), ..Default::default() });
                store.save()?;
                PairingSession::clear();
                let bound = crate::project_config::set_server(Path::new("."), server)?;
                let identity = identity_of(server).await;
                let mut payload = json!({
                    "state": "configured",
                    "server": server,
                    "l8b_toml": if bound { format!("l8b.toml (server bound to {server})") } else { format!("l8b.toml (already bound to {server})") },
                    "next": "Ready — verify with the `doctor` tool, then `deploy`.",
                });
                if let Some(id) = identity {
                    payload["identity"] = id;
                }
                if let Some(p) = project_id {
                    payload["token_project"] = json!(p);
                }
                return emit(out, &payload);
            }
            PairingPoll::Denied => {
                PairingSession::clear();
                bail!(crate::out::fail(
                    "pairing was denied on the server",
                    format!("start a new one: call setup with {{\"server\": \"{server}\"}}"),
                ));
            }
            PairingPoll::Expired => {
                PairingSession::clear();
                bail!(crate::out::fail(
                    "pairing code expired",
                    format!("start a new one: call setup with {{\"server\": \"{server}\"}}"),
                ));
            }
            PairingPoll::Pending => {
                if unix_now() >= deadline {
                    return emit(
                        out,
                        &json!({
                            "state": "pairing_pending",
                            "server": server,
                            "approval_url": session.approval_url(),
                            "next": "Not approved yet — remind the user to open approval_url, then call setup with {\"server\": <url>, \"complete\": true} again.",
                        }),
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(session.interval)).await;
            }
        }
    }
}

/// Validate the stored login for `server` and bind this workspace to it.
async fn bind(server: &str, out: &Out) -> Result<()> {
    let auth_header = CredentialStore::load().get(server).and_then(|c| c.auth_header());
    let Some(auth_header) = auth_header else {
        bail!(crate::out::fail(
            format!("not logged in to {server} — nothing to bind"),
            format!("pair first: call setup with {{\"server\": \"{server}\"}}"),
        ));
    };
    let client = auth::client_for(auth_header)?;
    let who = auth::api_get(&client, server, "/whoami").await.map_err(|e| {
        // keep the credential: a cookie login may just have expired; re-pairing replaces it
        crate::out::fail(
            format!("stored credential for {server} was rejected: {e}"),
            format!("pair again: call setup with {{\"server\": \"{server}\"}}"),
        )
    })?;
    let bound = crate::project_config::set_server(Path::new("."), server)?;
    emit(
        out,
        &json!({
            "state": "configured",
            "server": server,
            "identity": identity_json(&who),
            "l8b_toml": if bound { format!("l8b.toml (server bound to {server})") } else { format!("l8b.toml (already bound to {server})") },
            "next": "Ready — verify with the `doctor` tool, then `deploy`.",
        }),
    )
}

/// Best-effort /whoami for the stored credential; None (with no failure) when unreachable.
async fn identity_of(server: &str) -> Option<serde_json::Value> {
    let auth_header = CredentialStore::load().get(server).and_then(|c| c.auth_header())?;
    let client = auth::client_for(auth_header).ok()?;
    let who = auth::api_get(&client, server, "/whoami").await.ok()?;
    Some(identity_json(&who))
}

fn identity_json(who: &serde_json::Value) -> serde_json::Value {
    if who["kind"].as_str() == Some("token") {
        json!({
            "kind": "token",
            "name": who["name"].as_str().unwrap_or("unnamed"),
            "scope": who["scope"].as_str().unwrap_or("?"),
            "project_id": who["project_id"].as_str(),
        })
    } else {
        json!({"kind": "session", "username": who["username"].as_str().unwrap_or("?")})
    }
}

/// JSON payload out; human mode gets a two-line summary.
fn emit(out: &Out, payload: &serde_json::Value) -> Result<()> {
    out.ok(payload);
    if !out.json {
        println!("state: {}", payload["state"].as_str().unwrap_or("?"));
        if let Some(url) = payload["approval_url"].as_str() {
            println!("approval_url: {url}");
        }
        println!("next: {}", payload["next"].as_str().unwrap_or("?"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(server: &str, expires_at: i64) -> PairingSession {
        PairingSession {
            server: server.into(),
            device_code: "dc".into(),
            user_code: "L8B-ABC".into(),
            interval: 3,
            expires_at,
        }
    }

    #[test]
    fn compute_state_matrix() {
        let logins = vec!["https://a.io".to_string(), "https://b.io".to_string()];
        let one = vec!["https://solo.io".to_string()];
        let pending = session("https://new.io", unix_now() + 600);

        assert_eq!(
            compute_state(Some("https://a.io/"), &logins, None),
            SetupState::Configured { server: "https://a.io".into() }
        );
        assert_eq!(
            compute_state(Some("https://d.io"), &logins, None),
            SetupState::Misconfigured { server: "https://d.io".into() }
        );
        assert_eq!(
            compute_state(None, &logins, Some(&pending)),
            SetupState::PairingPending { server: "https://new.io".into() }
        );
        assert_eq!(compute_state(None, &one, None), SetupState::NeedsBinding { server: "https://solo.io".into() });
        assert_eq!(compute_state(None, &logins, None), SetupState::Ambiguous { choices: logins.clone() });
        assert_eq!(compute_state(None, &[], None), SetupState::Unconfigured);
        // An expired pairing is as good as none.
        let expired = session("https://new.io", unix_now() - 1);
        assert_eq!(compute_state(None, &[], Some(&expired)), SetupState::Unconfigured);
    }

    #[test]
    fn next_instructions_embed_the_tool_call() {
        assert!(next_instruction(&SetupState::Unconfigured).contains("{\"server\": \"<url>\"}"));
        assert!(
            next_instruction(&SetupState::Misconfigured { server: "https://d.io".into() })
                .contains("{\"server\": \"https://d.io\"}")
        );
        assert!(
            next_instruction(&SetupState::PairingPending { server: "https://new.io".into() })
                .contains("\"complete\": true")
        );
        assert!(
            next_instruction(&SetupState::NeedsBinding { server: "https://solo.io".into() })
                .contains("\"action\": \"bind\"")
        );
        assert!(
            next_instruction(&SetupState::Ambiguous { choices: vec!["https://a.io".into()] }).contains("https://a.io")
        );
        assert!(next_instruction(&SetupState::Configured { server: "https://a.io".into() }).contains("`doctor`"));
    }
}
