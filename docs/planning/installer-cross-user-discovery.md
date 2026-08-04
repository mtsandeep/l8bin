# Installer: cross-user install discovery

**Status:** known limitation, not solved. Revisit later.
**Scope:** `install.sh` — `update`, `certs`, and detection helpers (`detect_master_dir`, `detect_agent_dir`, `find_install_dir`).
**Related:** `docs/decisions.md` (install paths), `docs/planning/security-hardening.md` (privilege model).

## LiteBin's privilege requirements

**Root is not required.** The only hard dependency is access to the Docker daemon (`/var/run/docker.sock`), obtained by being root *or* by membership in the `docker` group (or rootless Docker). `ensure_docker()` in `install.sh` only checks `command -v docker` + `docker compose version` — it never checks `id -u`.

What root buys (all optional convenience, nothing functional):

| Concern | Root | Non-root (docker group) |
|---|---|---|
| Install location | `/opt/litebin` + `/etc/litebin/certs` | `~/litebin` + `~/litebin/certs` |
| Auto firewall | `configure_ufw` runs (gated on `id -u -eq 0`) | Skipped — prints manual "open these ports" |
| Privileged ports 80/443 | Bound by the Caddy **container** via the daemon | Same — daemon does the host bind, so fine without root |
| Everything else | Identical | Identical |

**Caveat:** `network_mode: host` workloads require a **rootful** Docker engine (see `docs/security.md` and `docs/faq.md`). That's a property of *how the daemon is installed* (rootful vs rootless), not of the litebin-installing user. Rootless Docker avoids root entirely but blocks host-network apps.

**Security note:** the `docker` group is effectively root-equivalent (a member can mount the host FS and become root). So "no root, use the docker group" is a *convenience*, not a real privilege reduction. Real reduction = rootless Docker + user namespace remap (`docs/planning/security-hardening.md`, idea #6).

**Bottom line:** LiteBin runs end-to-end without root/sudo given Docker daemon access. The cross-user discovery problem below is therefore a *file-permission* issue around where the install dir lives, not a "needs root to run" issue.

## The problem

LiteBin installs to one of two places:

- **root** → `/opt/litebin` (+ `/etc/litebin/certs`)
- **non-root** → `~/litebin` (+ `~/litebin/certs`)

When a *different* user later runs the installer to update or manage certs, detection must locate the existing install. Detection currently checks:

1. `/opt/litebin`
2. `${HOME}/litebin`
3. `${SUDO_USER}/litebin` — only when invoked via `sudo` from a non-root account

This works for the common cases (root-installed seen by any root context; own-home seen by the owner; root-installed seen via `sudo bash` thanks to `SUDO_USER`).

It **fails** in two situations:

1. **Direct-root updating a user-home install.** Root is logged in directly (no `sudo`), so `SUDO_USER` is unset and `HOME=/root`. `/home/<user>/litebin` is never checked. Result: misleading *"No LiteBin installation found"* and a high risk of a second install into `/opt/litebin`.

2. **Any unprivileged user looking for another user's home install.** On most distros `/home/<user>` is `0700`/`0750`, so a non-root user **cannot `stat`** another user's `~/litebin`. The kernel denies it — no amount of scanning fixes this from an unprivileged context.

## Why there is no "solid" universal solution

Cross-user discovery from an unprivileged context is **fundamentally unsolvable** without privilege escalation or a pre-agreed location. This is not a litebin bug; it's how Unix home-directory permissions work. We should not pretend otherwise (e.g. silently scanning `/home/*` and reporting "found nothing" when the truth is "permission denied").

## How other tools handle it

- **World-readable registry / marker file** (dpkg → `/var/lib/dpkg`, systemd → `/etc/systemd`): at install time, write the install dir to a fixed world-readable file (e.g. `/etc/litebin/install-location`). Any user reads it without scanning homes. Only root can write it, so it covers system/root installs; user-home installs stay private (mirrors `pip --user`).
- **One fixed canonical location** (Homebrew `/opt/homebrew`, rustup `~/.cargo`, nvm `~/.nvm`): sidestep discovery entirely by never supporting arbitrary locations. For litebin this would mean "master is always `/opt/litebin`, always root" — loses the non-root tryout path.
- **Explicit scope flag** (npm/pip/cargo `--global` vs user): the tool reads its own recorded prefix, not the filesystem.
- **Accept the limit + honest messaging** (pip, npm): don't hunt across `/home/*`; when detection fails, say plainly that installs in other users' homes aren't visible and suggest running as root or as the installing user.

## Candidate approaches to revisit

1. **Marker file (preferred candidate).** Root install writes `/etc/litebin/install-location` (world-readable) recording the master/agent dir. Detection checks the marker first, then falls back to today's hardcoded paths. Non-root installs can't register (correct: a non-root master is a personal/dev install, not a system one). Closest match to the unix idiom users expect; no home scanning.
2. **Honest fallback message.** When normal detection fails, print: the current user, that installs under other users' homes aren't visible to them, and suggest running as root or as the original installer. Cheap, zero risk — pair with (1).
3. **Drop the non-root master path** (not recommended). Force `/opt/litebin` + root always. Removes the whole class of ambiguity but loses the supported non-root install path.

## Current behavior (as of this writing)

- `update` / `certs` correctly detect `/opt/litebin` regardless of invoking user (`detect_master_dir` is not root-gated).
- If the resolved dir is not writable by the current user, `require_writable()` (in `install.sh`) prints a clear message with the exact `sudo bash -s …` command instead of a confusing failure.
- The two failure cases above are **not** handled: direct-root-vs-user-home, and any cross-user home discovery. Both fall through to a generic "not found" with no hint about other users.

## Decision needed when revisiting

- Do we want system installs to be discoverable by any user? If yes → marker file (approach 1).
- Do we accept that user-home installs are private to their owner (undiscoverable by others)? Recommended: yes — this matches `pip --user` and is the only honest answer given home-dir permissions.
