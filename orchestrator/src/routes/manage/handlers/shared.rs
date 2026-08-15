use litebin_common::types::{DeployType, ProjectStatus};

pub(super) fn can_attempt_full_stop(status: &ProjectStatus) -> bool {
    matches!(status, ProjectStatus::Running | ProjectStatus::Degraded | ProjectStatus::Stopping | ProjectStatus::Error)
}

pub(super) fn uses_compose_lifecycle(deploy_type: Option<&DeployType>) -> bool {
    deploy_type == Some(&DeployType::Compose)
}

#[cfg(test)]
mod tests {
    use super::{can_attempt_full_stop, uses_compose_lifecycle};
    use litebin_common::types::{DeployType, ProjectStatus};

    #[test]
    fn full_stop_accepts_retryable_runtime_states_only() {
        for status in [ProjectStatus::Running, ProjectStatus::Degraded, ProjectStatus::Stopping, ProjectStatus::Error] {
            assert!(can_attempt_full_stop(&status), "{status}");
        }

        for status in [
            ProjectStatus::Pending,
            ProjectStatus::Stopped,
            ProjectStatus::Deploying,
            ProjectStatus::Importing,
            ProjectStatus::Unconfigured,
            ProjectStatus::Completed,
        ] {
            assert!(!can_attempt_full_stop(&status), "{status}");
        }
    }

    #[test]
    fn one_service_lifecycle_routing_uses_deploy_type_not_service_count() {
        // Service count is deliberately not an input to this decision.
        assert!(uses_compose_lifecycle(Some(&DeployType::Compose)));
        assert!(!uses_compose_lifecycle(Some(&DeployType::Image)));
        assert!(!uses_compose_lifecycle(None));

        // Full stop is deliberately identity-based and remains retryable for
        // both deployment types, including a one-service background Compose.
        assert!(can_attempt_full_stop(&ProjectStatus::Running));
        assert!(can_attempt_full_stop(&ProjectStatus::Error));
    }
}
