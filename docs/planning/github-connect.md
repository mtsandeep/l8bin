# GitHub Connect & Deploy from Repo

**Status:** implemented (PAT + GitHub App manifest modes, manual + push auto-deploy)

Connect GitHub once on the dashboard, browse your repos, pick one — LiteBin commits a
generated workflow into the repo, writes `L8B_SERVER`/`L8B_TOKEN` as Actions secrets via
the GitHub API, triggers it via `workflow_dispatch`, and shows run status on the project
card. Builds run on GitHub-hosted runners through the existing `l8b` pipeline (Railpack
build → chunked image upload → `POST /deploy`); the VPS never builds.

This is roadmap item 13 ("GitHub App (seamless repo connect)"), pulled forward from
Future. It is the foundation for Phase 1 preview environments: the
`project_github_config` table, the App webhook receiver, and the GitHub client module
are shared with previews.

Full plan: [roadmap.md](roadmap.md)

## Why

- The Actions deploy path existed but required users to hand-write a workflow, create
  secrets, and wire everything themselves. Heroku/Vercel-style "pick a repo, deploy"
  is the highest-friction reducer left in the deploy story.
- No self-hosted PaaS at this price point does it without also building on the VPS.
  Building on GitHub runners keeps the litebin promise: the server just runs containers.

## User Experience

```
1. Settings → GitHub → "Connect with GitHub App"
   (GitHub shows a PREFILLED registration form — one confirm click)
2. Install the app on your account, pick repos            ← same screen as any integration
3. Home → Deploy → "GitHub repo" → browse repos (search, account picker)
4. Pick repo → branch, project id, port/compose, auto-deploy toggle → Link & deploy
5. LiteBin commits .github/workflows/litebin-deploy.yml + writes 2 repo secrets
6. First deploy dispatches automatically; badge on the project card tracks runs
```

PAT fallback (LAN instances): Settings → GitHub → "Use a personal access token" —
paste a classic PAT with `repo`+`workflow` scopes (or a fine-grained token with
Contents/Workflows write). Same downstream flow.

## Connection modes

| | GitHub App (primary) | PAT (fallback) |
|---|---|---|
| Setup | Manifest flow — prefilled form, one click + install | Paste token |
| Token lifetime | 1 h installation tokens, auto-refreshed | Until revoked/expired |
| Repo visibility | Repos the install selects (changeable on GitHub) | Everything the token sees |
| Requirements | Dashboard reachable from the browser; webhook URL reachable by GitHub for install lifecycle | Nothing extra — works on LAN |
| Rate limit | 15k req/hr per installation | 5k req/hr |

No GitHub approval/review anywhere: manifest-flow apps are private, owned by the
self-hoster, installed only on their own repos. (Marketplace review exists only for
public listings we don't need.)

## Data Model

Migration `0032_github_connect.sql`: `github_connection` (singleton — mode, app creds or
PAT, pending manifest nonce, status), `github_installations` (GitHub installation id →
account, selection, status), `project_github_config` (project ↔ repo_full_name/repo_id,
branch, workflow_path, deploy_kind, workflow_inputs JSON, auto_deploy,
deploy_token_id, installation_id, denormalized last_run_*).

PEM/PAT stored plaintext — same tradeoff as `cloudflare_api_token` (at-rest encryption
is a [security-hardening.md](security-hardening.md) item).

## Generated workflow

`orchestrator/src/github/workflow.rs` renders `.github/workflows/litebin-deploy.yml`:
`workflow_dispatch` + optional `push` (branch-filtered) triggers, per-project
concurrency group with cancel-in-progress, pinned `mtsandeep/l8bin-action@v0.2.0`,
secrets referenced — never inlined. Managed-file policy: LiteBin rewrites it on every
config change (banner warns); the auto-deploy toggle IS the push block being present.

App permissions requested: `metadata:read, contents:write, workflows:write,
actions:write, secrets:write`. No `default_events`: `installation` lifecycle events are
delivered to Apps automatically (subscribing explicitly fails GitHub's validation), and
repo visibility is handled by live polling of `GET /app/installations`. The webhook is
omitted entirely when the dashboard host isn't publicly reachable (localhost / private
IPs) — GitHub validates webhook reachability at registration, and installation status
falls back to the same polling.

## API Endpoints

```
GET/DELETE /github/connection            admin   status / disconnect (revokes link tokens)
POST      /github/manifest               admin   build manifest → setup_url
POST      /github/manifest/exchange      admin   {code} → app credentials stored
POST      /github/pat                    admin   {token} → validated, stored
GET       /github/installations          manage  list + sync installations
GET       /github/repos?installation_id= manage  repo list (installation or PAT scoped)
GET       /github/repos/{owner}/{repo}   manage  detail + compose/Dockerfile detection
GET       /github/repos/{o}/{r}/branches manage  branch picker
GET/PUT/PATCH/DELETE /projects/{id}/github/config  read/manage  link CRUD
POST      /projects/{id}/github/rotate-token      manage  new deploy token → secret → revoke old
POST      /projects/{id}/github/deploy            manage  workflow_dispatch → 202
GET       /projects/{id}/github/runs     read    run history (proxied, 10s client poll)
GET       /github/status                 read    bulk latest-run per link (20s server cache)
POST      /github/webhook                public  HMAC X-Hub-Signature-256, installation lifecycle
```

Link PUT is all-or-nothing: mint project-scoped deploy token (hash-only) → PUT secrets
(sealed box, dryoc) → PUT workflow file (sha-aware) → upsert row. Any failure rolls the
token back. Rotation overwrites the secret before revoking the old token so in-flight
runs never lose auth.

## Edge Cases

| Scenario | Handling |
|---|---|
| Protected default branch (PUT 403) | Typed `Forbidden` error; UI guidance to pick another branch or allow the app/PAT |
| User-edited workflow file | Overwritten (managed file; banner in the file says so) |
| PAT 401 / app uninstalled | Connection → `degraded` + status_message; webhook keeps installations fresh |
| Fine-grained PAT (no scope header) | Accepted with a warning; permissions surface at first write |
| Secondary rate limits | Typed `RateLimited`; repo listing paginated 100/page |
| Private repos | Info line: builds count against GitHub Actions minutes |
| Non-default branch | Workflow written with `?ref=` + dispatch on that branch |
| Compose + Dockerfile both present | Compose preferred; user can force single in the form |
| Same repo linked to two projects | Allowed; concurrency group is per-project |
| Repo renamed/transferred | Follow-up: auto-heal via stored `repo_id` (not yet wired) |

## Files

| Area | Files |
|---|---|
| Client | `orchestrator/src/github/{mod,auth,repos,actions,manifest,workflow}.rs` |
| Routes | `orchestrator/src/routes/github/{mod,connect,repos,project_link,runs,webhook}.rs` |
| Migration | `orchestrator/src/db/migrations/0032_github_connect.sql` |
| Dashboard | `settings/GitHubPanel.tsx`, `settings/GithubCallbackPage.tsx`, `GitHubRepoBrowser.tsx`, `GitHubDeployForm.tsx`, `ProjectCard/GitHubBadge.tsx`, `ProjectCard/GitHubRunsModal.tsx`, `hooks/useGithubStatus.ts` |
| Action | [mtsandeep/l8bin-action](https://github.com/mtsandeep/l8bin-action) v0.2.0 (compose/services inputs, `url` output) |

## Verification

- Unit: sealed-box roundtrip, workflow renders (all mode combos), split_repo,
  webhook HMAC sign/verify + reject, manifest setup_url encoding.
- E2E: PAT → browse → link branch of `test-apps/node-react-demo` → workflow + secrets
  present in repo → deploy green → project serves → badge green; kill token → degraded.
- Compose E2E: `test-apps/multi-service` → compose detected, deploy succeeds.
- App mode: manifest flow on a real account → install on one repo → exactly that repo
  listed → uninstall → flagged.
- Auto-deploy: toggle on → push → runs with no server webhook; toggle off → push → nothing.

## Follow-ups

- Repo rename auto-heal via `repo_id` (`GET /repositories/{id}` on 404).
- PR preview environments (Phase 1) build directly on this: extend
  `/github/webhook` with `pull_request` events, add the `previews` table.
- Run-log import into the dashboard (currently link-out to GitHub).
