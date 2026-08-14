use bollard::query_parameters::LogsOptions;
use futures_util::StreamExt;

use super::super::DockerManager;

const HOST_NETWORK_STABILIZATION_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
const HOST_NETWORK_STABILIZATION_POLL: std::time::Duration = std::time::Duration::from_millis(200);
const STARTUP_LOG_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const STARTUP_LOG_TAIL_LINES: usize = 40;
pub(crate) const STARTUP_LOG_MAX_CHARS: usize = 4_096;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StartupProcessState {
    RunningOrUnknown,
    Exited(Option<i64>),
}

pub(crate) fn should_stabilize_startup(host_network: bool, is_oneshot: bool, is_managed_proxy: bool) -> bool {
    host_network && !is_oneshot && !is_managed_proxy
}

pub(crate) fn startup_process_state(running: Option<bool>, exit_code: Option<i64>) -> StartupProcessState {
    if running == Some(false) { StartupProcessState::Exited(exit_code) } else { StartupProcessState::RunningOrUnknown }
}

pub(crate) fn sanitize_startup_log_chunks<'a>(chunks: impl IntoIterator<Item = &'a str>) -> String {
    use std::collections::VecDeque;

    #[derive(Clone, Copy)]
    enum EscapeState {
        Text,
        Escape,
        Csi,
        Osc,
        OscEscape,
    }

    let mut state = EscapeState::Text;
    let mut tail = VecDeque::with_capacity(STARTUP_LOG_MAX_CHARS);
    let push = |character: char, tail: &mut VecDeque<char>| {
        if tail.len() == STARTUP_LOG_MAX_CHARS {
            tail.pop_front();
        }
        tail.push_back(character);
    };

    for chunk in chunks {
        for character in chunk.chars() {
            match state {
                EscapeState::Text => match character {
                    '\u{1b}' => state = EscapeState::Escape,
                    '\r' => push('\n', &mut tail),
                    '\n' | '\t' => push(character, &mut tail),
                    value if value.is_control() => {}
                    value => push(value, &mut tail),
                },
                EscapeState::Escape => {
                    state = match character {
                        '[' => EscapeState::Csi,
                        ']' => EscapeState::Osc,
                        _ => EscapeState::Text,
                    };
                }
                EscapeState::Csi => {
                    if ('@'..='~').contains(&character) {
                        state = EscapeState::Text;
                    }
                }
                EscapeState::Osc => match character {
                    '\u{7}' => state = EscapeState::Text,
                    '\u{1b}' => state = EscapeState::OscEscape,
                    _ => {}
                },
                EscapeState::OscEscape => {
                    state = if character == '\\' { EscapeState::Text } else { EscapeState::Osc };
                }
            }
        }
    }

    tail.into_iter().collect::<String>().trim().to_string()
}

impl DockerManager {
    pub(crate) async fn wait_for_host_network_startup(
        &self,
        container_id: &str,
        service_name: &str,
    ) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + HOST_NETWORK_STABILIZATION_WINDOW;
        loop {
            let info = self.docker.inspect_container(container_id, None).await?;
            let state = info.state.as_ref();
            match startup_process_state(state.and_then(|state| state.running), state.and_then(|state| state.exit_code))
            {
                StartupProcessState::RunningOrUnknown => {}
                StartupProcessState::Exited(exit_code) => {
                    let exit_context = exit_code
                        .map(|code| format!("exit code {code}"))
                        .unwrap_or_else(|| "exit code unavailable".to_string());
                    let logs = tokio::time::timeout(STARTUP_LOG_FETCH_TIMEOUT, self.startup_log_tail(container_id))
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or_default();
                    if logs.is_empty() {
                        anyhow::bail!(
                            "host-network service '{}' container '{}' exited during the {}s startup stabilization window ({})",
                            service_name,
                            container_id,
                            HOST_NETWORK_STABILIZATION_WINDOW.as_secs(),
                            exit_context
                        );
                    }
                    anyhow::bail!(
                        "host-network service '{}' container '{}' exited during the {}s startup stabilization window ({}); recent logs:\n{}",
                        service_name,
                        container_id,
                        HOST_NETWORK_STABILIZATION_WINDOW.as_secs(),
                        exit_context,
                        logs
                    );
                }
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(());
            }
            tokio::time::sleep(HOST_NETWORK_STABILIZATION_POLL.min(deadline.saturating_duration_since(now))).await;
        }
    }

    async fn startup_log_tail(&self, container_id: &str) -> anyhow::Result<String> {
        let options =
            LogsOptions { stdout: true, stderr: true, tail: STARTUP_LOG_TAIL_LINES.to_string(), ..Default::default() };
        let mut stream = self.docker.logs(container_id, Some(options));
        let mut chunks = Vec::new();
        while let Some(result) = stream.next().await {
            chunks.push(result?.to_string());
        }
        Ok(sanitize_startup_log_chunks(chunks.iter().map(String::as_str)))
    }
}
