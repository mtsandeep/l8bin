use std::collections::HashSet;

/// Options that control how `start_services` behaves.
#[derive(Default)]
pub struct StartServicesOpts {
    /// Always remove and recreate containers (skip fast-path docker start).
    pub force_recreate: bool,

    /// Ensure images exist for services that will be created (no existing
    /// container). Callers that pre-pull (deploy flows) pass false.
    pub pull_images: bool,

    /// Force a registry pull even when the image exists locally (redeploy).
    /// With force_pull false, present images are a cheap local inspect.
    pub force_pull: bool,

    /// Only start these services. None = all services.
    pub services: Option<HashSet<String>>,

    /// Connect the orchestrator container to the project network (needed for proxy).
    pub connect_orchestrator: bool,

    /// On failure, stop and remove all containers started in this call.
    pub rollback_on_failure: bool,

    /// Skip the trailing 2s DNS wait (waker only): the readiness probe is the precise version of that wait.
    pub skip_dns_wait: bool,
}
