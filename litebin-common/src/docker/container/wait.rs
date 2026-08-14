use super::super::DockerManager;

impl DockerManager {
    /// Wait for a container to become healthy (polls inspect every 500ms, timeout 60s).
    /// Returns Ok if healthy, or the last error if it becomes unhealthy or times out.
    /// When `expect_healthcheck` is true, keeps polling even if health is None (first
    /// check hasn't run yet). When false, returns immediately if no healthcheck exists.
    pub async fn wait_for_healthy(&self, container_id: &str, expect_healthcheck: bool) -> anyhow::Result<()> {
        use bollard::models::HealthStatusEnum;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let info = self.docker.inspect_container(container_id, None).await?;
            let health = info.state.as_ref().and_then(|s| s.health.as_ref());
            match health {
                None => {
                    if !expect_healthcheck {
                        return Ok(()); // No healthcheck defined
                    }
                    // Healthcheck exists but first check hasn't run yet — keep polling
                }
                Some(h) => match &h.status {
                    Some(HealthStatusEnum::HEALTHY) => return Ok(()),
                    Some(HealthStatusEnum::UNHEALTHY) => {
                        let log_msg =
                            h.log.as_ref().and_then(|logs| logs.last()).and_then(|l| l.output.as_deref()).unwrap_or("");
                        anyhow::bail!("container unhealthy: {}", log_msg);
                    }
                    _ => {} // EMPTY, NONE, STARTING — keep polling
                },
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("healthcheck timeout after 60s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Wait until a one-shot container exits successfully (exit code 0).
    /// Polls inspect every 500ms with a 10-minute timeout (migrations can be slow).
    pub async fn wait_for_completed_successfully(&self, container_id: &str) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
        loop {
            let info = self.docker.inspect_container(container_id, None).await?;
            let state = info.state.as_ref();
            let running = state.and_then(|s| s.running).unwrap_or(false);
            if !running {
                let exit_code = state.and_then(|s| s.exit_code).unwrap_or(-1);
                if exit_code == 0 {
                    return Ok(());
                }
                anyhow::bail!("one-shot container exited with code {}", exit_code);
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("one-shot container did not exit within 600s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Exit code of a container if it is not running; `None` if still running or unknown.
    pub async fn container_exit_code(&self, container_id: &str) -> anyhow::Result<Option<i64>> {
        let info = self.docker.inspect_container(container_id, None).await?;
        let state = info.state.as_ref();
        let running = state.and_then(|s| s.running).unwrap_or(false);
        if running {
            return Ok(None);
        }
        Ok(state.and_then(|s| s.exit_code))
    }

    /// Wait for a container to have a valid IP address on its network (not "invalid" or empty).
    /// Docker sometimes assigns "invalid" IP briefly after container creation.
    /// Polls every 200ms, timeout 10s.
    pub async fn wait_for_network_ready(&self, container_id: &str) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let info = self.docker.inspect_container(container_id, None).await?;
            let has_valid_ip = info
                .network_settings
                .as_ref()
                .and_then(|ns| ns.networks.as_ref())
                .map(|nets| {
                    nets.values().any(|net| {
                        let ip = net.ip_address.as_deref().unwrap_or("");
                        !ip.is_empty() && ip != "invalid"
                    })
                })
                .unwrap_or(false);

            if has_valid_ip {
                return Ok(());
            }

            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("network readiness timeout after 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
}
