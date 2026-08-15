mod delete;
mod helpers;
mod opts;
mod recreate;
mod start;
mod stop;

pub use delete::delete_all_services;
pub(crate) use helpers::apply_remote_batch_failure_metadata;
pub(in crate::routes::manage) use helpers::{approved_docker_observe_requesters, proxy_needed_after_stop};
pub use opts::StartServicesOpts;
pub use recreate::recreate_services;
pub use start::start_services;
pub use stop::stop_services;
