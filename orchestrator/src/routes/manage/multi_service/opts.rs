use std::collections::HashSet;

/// Options that control how `start_services` behaves.
#[derive(Default)]
pub struct StartServicesOpts {
    /// Always remove and recreate containers (skip fast-path docker start).
    pub force_recreate: bool,

    /// Pull images before starting (for fresh deploys).
    pub pull_images: bool,

    /// When pull_images is true, force_pull controls whether to always pull from
    /// the registry (true) or skip the pull if the image exists locally (false).
    pub force_pull: bool,

    /// Only start these services. None = all services.
    pub services: Option<HashSet<String>>,

    /// Connect the orchestrator container to the project network (needed for proxy).
    pub connect_orchestrator: bool,

    /// On failure, stop and remove all containers started in this call.
    pub rollback_on_failure: bool,
}
