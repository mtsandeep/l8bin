//! Network timeout taxonomy — the single home for outbound HTTP bounds.
//!
//! Every reqwest client / per-request timeout in the workspace references a
//! constant here (or a wake-policy constant in [crate::wake_wait] for the
//! waker's retry machinery). Values are deliberate — see each class:
//!
//! - Connect bounds: how fast we abandon establishing a connection.
//!   Localhost admin APIs fail in milliseconds when healthy; remote links
//!   (third-party APIs, possibly slow node VPSes) get progressively more slack.
//! - Totals cap a whole request+response. Destination classes differ:
//!   local admin API < third-party API < agent reads < container lifecycle
//!   < image work (pulls and tar streaming legitimately take minutes).
//!
//! Wake/proxy retry policy (ticks, attempt caps, failure windows) lives in
//! [crate::wake_wait] next to the machinery that uses it.

use std::time::Duration;

// ---------------------------------------------------------------------------
// Connect bounds
// ---------------------------------------------------------------------------

/// Localhost admin APIs (Caddy admin, public-IP probes).
pub const CONNECT_LOCAL: Duration = Duration::from_secs(2);

/// Third-party APIs and LAN/WAN services (GitHub, Cloudflare).
pub const CONNECT_REMOTE: Duration = Duration::from_secs(5);

/// mTLS node clients — remote nodes can sit behind slow links.
pub const CONNECT_NODE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Per-destination totals
// ---------------------------------------------------------------------------

/// Caddy admin API (`/load`, `/config/`) — localhost, small JSON payloads.
pub const CADDY_ADMIN_TOTAL: Duration = Duration::from_secs(15);

/// Third-party REST APIs (GitHub, Cloudflare DNS).
pub const API_THIRD_PARTY_TOTAL: Duration = Duration::from_secs(30);

/// Per-provider public-IP probe — cycles to the next provider on failure.
pub const PUBLIC_IP_PROBE_TOTAL: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Agent API classes (orchestrator → agent, and agent → master reports)
// ---------------------------------------------------------------------------

/// Control-plane pushes (health, register, meta, caddy sync) — must fail fast
/// so a dead node can't stall sync loops.
pub const AGENT_CONTROL_PLANE: Duration = Duration::from_secs(10);

/// Default for agent reads/probes — also the pooled node client's fallback.
pub const AGENT_READ: Duration = Duration::from_secs(60);

/// Container lifecycle calls — bounded by stop grace periods and startup stabilization.
pub const AGENT_LIFECYCLE: Duration = Duration::from_secs(300);

/// Image work — batch-run pulls images synchronously; /images/load streams tars.
pub const AGENT_IMAGE_WORK: Duration = Duration::from_secs(1800);

/// Fan-out health probes and agent → master reports (wake/activity) — fire-and-forget
/// paths that must never accumulate lag.
pub const AGENT_REPORT: Duration = Duration::from_secs(5);
