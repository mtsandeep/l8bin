# Security Hardening (Deferred)

Security work we've considered, analyzed, and chosen to defer. Each item has an honest cost/benefit assessment so future work can pick up where this analysis left off.

This doc is **not** an active roadmap. It's a parking lot for ideas that didn't justify implementation in litebin's current positioning but might matter later.

---

## Threat Model (Clarified)

LiteBin's model is **compromised-app containment**, not multi-tenant user isolation.

- **One trusted admin** deploys many apps.
- **Apps are treated as untrusted** — they may be compromised via CVEs, supply-chain attacks, or misconfiguration.
- A compromised app must not escape its container, attack neighbor apps, or reach the management plane.
- The admin deploying the app is trusted — they can give an app whatever it needs, but the app shouldn't be able to do more than what was granted.

This is meaningfully different from Coolify/CapRover/Dokku (which don't sandbox apps from each other) but stops short of multi-tenant platforms like Fly.io/Render (which use VM-level isolation). It's the right model for a self-hosted PaaS where one person runs many apps on their own server.

---

## What's Already Done

These are landed and form the current security baseline. Future hardening proposals should not re-litigate them.

### Container isolation
- **Volume path translation** — user compose paths (`./data`) are translated to `projects/{project_id}/...` and path escapes are rejected. The highest-value sandboxing primitive litebin provides.
- **`cap_drop: ALL`** baseline with minimal whitelist: `CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `FSETID`, `SETGID`, `SETUID`, `NET_BIND_SERVICE`, `KILL`. Caps that enable escape (`SYS_ADMIN`, `SYS_PTRACE`, `SYS_MODULE`, `NET_ADMIN`, `DAC_READ_SEARCH`) and `NET_RAW` (packet sniffing, ARP spoofing) remain dropped.
- **`no-new-privileges`** — setuid binaries inside the image cannot escalate.
- **`pids_limit: 4096`** — fork-bomb protection.
- **Memory + CPU limits** — per-project defaults with overrides.
- **Per-project networks for multi-service** — `litebin-{project_id}` network per compose project. Services within a project can reach each other; cross-project traffic is blocked.
- **Docker socket stripping** — compose-declared docker.sock mounts are removed unless the project has `allow_docker_access` enabled.

### Network isolation (current state)
- All management containers (orchestrator, dashboard, caddy) and all single-service apps share `litebin-network`.
- Multi-service projects get isolated `litebin-{project_id}` networks.
- The orchestrator's `/internal/*` endpoints are restricted by Caddy routing (poke subdomain) and the public API requires session or deploy-token auth.

---

## Deferred Ideas

### 1. Core-network isolation — `litebin-core-network`

**Idea:** Move orchestrator + dashboard to a separate `litebin-core-network`. Caddy bridges both networks (must be on both to route public traffic to apps and to orchestrator). Apps stay on `litebin-network`.

**What it buys:**
- A compromised single-service app can no longer `curl http://orchestrator:5080/...` directly via Docker DNS.

**What it does NOT buy:**
- Does not protect against orchestrator CVEs (orchestrator is still reachable via Caddy's public routes — that's its real attack surface).
- Does not protect against Caddy compromise (Caddy sits on both networks, so an RCE there dissolves the boundary).
- Does not stop lateral movement between single-service apps (they all still share `litebin-network`).
- Does not help multi-service apps (already isolated).

**Why deferred:**
- Marginal benefit — closes one path that orchestrator auth already defends.
- Real complexity cost — 5+ files modified (docker-compose.yml, install.sh, install-windows.ps1, .env.example, docs), install script changes are high-stakes for production users, migration risk for existing deployments.
- Coolify/CapRover/Dokku don't bother and they work fine.
- **Caddy bridge problem**: Caddy is internet-exposed and runs complex config-parsing code, so it's a likely attack target. A Caddy RCE instantly defeats the isolation — the boundary is "isolated except through Caddy," which is weak.

**If reconsidered later:** Also evaluate [per-project networks for single-service apps](#2-per-project-networks-for-single-service-apps) instead. That's the bigger, more meaningful version of this idea.

---

### 2. Per-project networks for single-service apps

**Idea:** Every single-service project gets its own network (like multi-service projects already do). Caddy joins each per-project network dynamically (same as it already does for multi-service). The orchestrator stays on `litebin-network` alone — no apps share it.

**What it buys:**
- Closes the **lateral movement vector** — currently the most meaningful gap in the model. Today, a compromised app can `curl http://litebin-{other-project}:{port}/...` to attack another project on the same Docker network. With per-project networks, that attack fails at DNS resolution.

**What it does NOT buy:**
- Does not by itself isolate apps from the orchestrator (would need to combine with [core-network isolation](#1-core-network-isolation--litebin-core-network)).
- Does not protect against vulnerabilities in apps that are intentionally exposed to the internet.

**Cost:**
- Changes the single-service deploy path. Today single-service apps use `network_mode: Some(self.network)` in [container.rs:438](../../litebin-common/src/docker/container.rs#L438); would need to create a per-project network and attach Caddy.
- More networks to manage, but Docker handles this fine.
- Existing deployments need migration logic.

**Why deferred:**
- Higher value than core-network isolation but also higher complexity.
- Current single-service users haven't reported lateral-movement concerns.
- Would naturally combine with core-network isolation as a single coordinated change.

**If reconsidered later:** This is the change that actually delivers on the compromised-app containment promise. Pair with core-network isolation for a complete story.

---

### 3. Read-only root filesystem

**Idea:** Run containers with `read_only: true` + tmpfs for `/tmp`, `/run`, etc. Webshells and other file-based persistence can't survive a container restart.

**What it buys:**
- A PHP RCE that writes a webshell to `/var/www/html/uploads/shell.php` fails with EROFS (unless the path is bind-mounted).
- Forces immutability for stateless apps — hidden state becomes visible.

**Cost:**
- ~30% of images don't support it without per-image tuning (PHP apps need `/var/log/apache2`, Postgres needs `/var/run/postgresql`, etc.).
- The user has to figure out tmpfs paths per app.

**Already works today** — `read_only` and `tmpfs` are honored by litebin's compose parsing. No code change needed. The work is purely documentation: a "Hardening your apps" section in user-facing docs explaining the trade-off and showing example patterns.

**Why deferred:**
- The toggle UX is a trap — users who flip it on without tuning tmpfs will hit confusing failures.
- Compose-native — no need to reimplement as a litebin feature.
- Documentation effort alone would deliver the value; nobody has asked for it.

**If reconsidered later:** Document, don't build a toggle. A "Hardening your apps" doc page is the right intervention.

---

### 4. Custom seccomp profile

**Idea:** Ship a seccomp profile stricter than Docker's default. Docker's default blocks ~50 syscalls; a hardened profile blocks another ~30 without breaking typical apps.

**What it buys:**
- Catches escape primitives that caps miss (e.g., `unshare`, `clone` with new namespaces, `keyctl`).

**Cost:**
- Profile maintenance — kernel adds syscalls, apps find new patterns. A profile that's too strict breaks apps subtly.
- Diminishing returns past `cap_drop ALL` + Docker's default seccomp.

**Why deferred:** The current baseline already removes the high-value targets. Adding seccomp is engineering effort for marginal benefit.

**If reconsidered later:** Adopt an upstream-maintained profile (e.g., the NCSC hardened Docker profile) rather than writing one from scratch.

---

### 5. AppArmor / SELinux profile

**Idea:** Mandatory Access Control at the LSM layer. Catches what caps and seccomp miss. Kernel-level enforcement survives even capability mistakes.

**What it buys:**
- Defense-in-depth that doesn't depend on container runtime behavior.
- Catches zero-day container escapes that capabilities wouldn't block.

**Cost:**
- LSM-specific (AppArmor on Ubuntu/Debian, SELinux on RHEL/Fedora). Different profiles for different distros.
- Profile maintenance — same problem as seccomp.
- Most self-hosted PaaS users run on Ubuntu, so AppArmor would be the focus.

**Why deferred:** Adds significant complexity for a threat model where apps are already well-contained by caps + volume translation.

**If reconsidered later:** Pair with custom seccomp as a coordinated hardening push, only if litebin's positioning shifts toward hosting untrusted code.

---

### 6. User namespace remapping (`userns-remap`)

**Idea:** Docker daemon configuration that maps container UID 0 to an unprivileged host user (e.g., UID 100000). Container root is no longer host root. The structural fix — capabilities become nearly meaningless because they apply within the user namespace, not the host's.

**What it buys:**
- A container escape gives the attacker UID 100000 on the host — still unprivileged. Cannot modify host files, kill host processes, or mount filesystems.
- Limits blast radius regardless of which caps are granted.

**Limitations:**
- **Docker daemon setting, not a per-container feature** — all-or-nothing on the host. Cannot be enabled from inside litebin.
- **Does not work for Windows containers** — user namespaces don't exist on Windows.
- **Does not work cleanly in Docker Desktop** — only inside the WSL2 VM, and the GUI doesn't expose it.
- **Works cleanly on Linux servers** — the actual production target.
- Breaks bind mount ownership semantics — `chown_bind_mounts` in [container.rs](../../litebin-common/src/docker/container.rs) would need to use mapped UIDs.

**Why deferred:** Not a litebin feature — it's a deployment recommendation. Worth mentioning in install docs as "if you want stronger isolation, enable userns-remap on your Docker daemon" with a pointer to the trade-offs.

**If reconsidered later:** Document as a deployment recommendation. Don't try to manage it from inside litebin.

---

### 7. Audit logging

**Idea:** Append-only log of container actions (started, stopped, exec'd, signals sent, filesystem writes to sensitive paths). After a breach, this is what tells you what happened.

**What it buys:**
- Post-incident forensics — without this, you can't answer "what did the attacker do?".
- Anomaly detection hooks (unusual `docker exec` patterns, etc.).

**Cost:**
- Storage — append-only logs grow forever without rotation.
- Volume — every container action generates an event; need to be selective about what's audited.
- Where to store — orchestrator DB? Separate audit log file? External system?

**Why deferred:** Operational hardening, not preventive. Useful after a breach but doesn't prevent one. Lower priority than preventive controls.

**If reconsidered later:** Pairs naturally with the [Notifications](notifications.md) planning doc — both are event-driven and use a similar outbox pattern.

---

### 8. Disk quotas per project

**Idea:** XFS project quotas on `projects/{project_id}/`, or `du`-based polling with deploy rejection when a project exceeds its limit.

**What it buys:**
- One project's runaway logs or uploads can't fill the host disk and take down every other project.
- Forces explicit capacity planning.

**Cost:**
- XFS quotas require the host filesystem to be XFS with `prjquota` enabled — not always the case.
- `du`-based polling is approximate and has race conditions.
- Per-project quota tracking adds DB state and dashboard UX.

**Why deferred:** The current `pids_limit` + memory + CPU limits cover the noisy-neighbor problem for CPU/RAM. Disk is the remaining gap, but no users have hit it.

**If reconsidered later:** Start with `du`-based polling (simpler, no FS requirements) and warn-only mode before enforcing.

---

## Decision Framework for Future Work

When considering new security hardening, apply this filter:

| Question | If "no" → |
|---|---|
| Does it close a real attack vector that auth/network design doesn't already cover? | Probably not worth it. |
| Is the threat multi-tenant (untrusted users) or compromised-app? litebin is the latter. | Don't add multi-tenant controls. |
| Can the user already do this via compose-native fields? | Document, don't build a toggle. |
| Does it require touching install scripts? | High bar — migration risk is real. |
| Does it weaken isolation elsewhere (e.g., Caddy bridge)? | Reconsider the design. |

The current baseline (volume translation + cap restrictions + multi-service per-project networks) already provides meaningfully stronger compromised-app containment than Coolify/CapRover/Dokku. That's litebin's security differentiator. Further hardening has diminishing returns until the threat model shifts.
