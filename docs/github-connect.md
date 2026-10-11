# GitHub Connect & Deploy from Repo

Connect GitHub once on the dashboard, browse your repos, pick one — LiteBin commits a
generated workflow into the repo, writes the shared `L8B_SERVER` secret and a
per-project `L8B_TOKEN_<PROJECT>` secret via the GitHub API, triggers it via
`workflow_dispatch`, and shows run status on the project card. Builds run on
GitHub-hosted runners through the existing `l8b` pipeline (Railpack build → chunked
image upload → `POST /deploy`); the VPS never builds.

## User Experience

```
1. Settings → GitHub → "Connect with GitHub App"
   (GitHub shows a PREFILLED registration form — one confirm click)
2. Install the app on your account, pick repos            <- same screen as any integration
3. Home → Deploy → "GitHub repo" → browse repos (search, account picker)
4. Pick repo → branch, project id, folder, auto-deploy toggle → Link & deploy
5. LiteBin commits .github/workflows/litebin-<project>-deploy.yml + writes the secrets
6. First deploy dispatches automatically; the GitHub icon on the project card tracks runs
```

Existing projects link a repo from the card's settings (gear → General → GitHub repo)
instead of the deploy dialog — the dialog only creates new projects. The project card
shows a GitHub status icon (queued / running / success / failure / cancelled); clicking
it opens run history with Deploy now, the auto-deploy toggle, token rotation, and unlink.

PAT fallback (LAN instances): Settings → GitHub → "Use a personal access token" —
paste a classic PAT with `repo`+`workflow` scopes (or a fine-grained token with
Contents/Workflows write). Same downstream flow.

## Connection Modes

| | GitHub App (primary) | PAT (fallback) |
|---|---|---|
| Setup | Manifest flow — prefilled form, one click + install | Paste token |
| Token lifetime | 1 h installation tokens, auto-refreshed | Until revoked/expired |
| Repo visibility | Repos the install selects (changeable on GitHub) | Everything the token sees |
| Requirements | Dashboard reachable from the browser; webhook URL reachable by GitHub for install lifecycle | Nothing extra — works on LAN |
| Rate limit | 15k req/hr per installation | 5k req/hr |

No GitHub approval/review anywhere: manifest-flow apps are private, owned by the
self-hoster, installed only on their own repos.

**Multiple accounts:** private apps only install on their owning account, so covering
several GitHub accounts means one app each — connections coexist with independent
installations, per-account repo lists, and per-connection disconnect. Installations
stay in sync by polling and webhook; uninstalling on GitHub flips the card back to
"install" automatically.

## Data Model

Migrations `0032_github_connect.sql` + `0033_multi_app_connections.sql`:
`github_connection` (multi-row — one per app/PAT, with label), `github_installations`
(installation id → account, selection, status, connection_id), and
`project_github_config` (project ↔ repo, branch, workflow_path, deploy_kind,
workflow_inputs JSON, auto_deploy, deploy_token_id, installation_id, connection_id,
denormalized last_run_*).

PEM/PAT stored plaintext — same tradeoff as `cloudflare_api_token` (at-rest encryption
is a [security-hardening.md](planning/security-hardening.md) item).

## The Generated Workflow

`orchestrator/src/github/workflow.rs` renders
`.github/workflows/litebin-{project_id}-deploy.yml` — one workflow file per project
per repo:

- `workflow_dispatch` + optional `push` trigger — branch-filtered **and
  path-filtered** (`paths: [<folder>/**]` for subfolder links; root links
  `paths-ignore` their own workflow file so re-links never self-trigger)
- per-project concurrency group with cancel-in-progress
- pinned `mtsandeep/l8bin-action@v0.2.0`, shared `L8B_SERVER` + per-project
  `L8B_TOKEN_<PROJECT>` secrets referenced — never inlined

Managed-file policy: LiteBin rewrites the file on every config change (the banner in
the file says so); the auto-deploy toggle is just the push block being present.
Unchanged re-links skip no-op commits. `L8B_SERVER` is removed only when the last
project linked to that repo unlinks.

App permissions requested: `metadata:read, contents:write, workflows:write,
actions:write, secrets:write`. No `default_events`: installation lifecycle events are
delivered to Apps automatically, and repo visibility is handled by live polling of
`GET /app/installations`. The webhook is omitted entirely when the dashboard host
isn't publicly reachable (localhost / private IPs) — GitHub validates webhook
reachability at registration.

## API Endpoints

```
GET       /github/connection             admin   aggregate status
GET       /github/connections            admin   list connections + installations
DELETE    /github/connections/{id}       admin   per-connection disconnect
POST      /github/manifest               admin   raw manifest JSON (the dashboard POSTs it to github.com/settings/apps/new)
POST      /github/manifest/exchange      admin   {code} -> app credentials stored
POST      /github/pat                    admin   {token} -> validated, stored
POST      /github/app                    admin   attach an existing app (App ID + .pem)
GET       /github/installations          manage  list + sync installations
GET       /github/repos?installation_id= manage  repo list (installation or PAT scoped)
GET       /github/repos/{owner}/{repo}   manage  detail + compose/Dockerfile detection
GET       /github/repos/{o}/{r}/branches manage  branch picker
GET/PUT/PATCH/DELETE /projects/{id}/github/config  read/manage  link CRUD
POST      /projects/{id}/github/rotate-token      manage  new deploy token -> secret -> revoke old
POST      /projects/{id}/github/deploy            manage  workflow_dispatch -> 202
GET       /projects/{id}/github/runs     read    run history (proxied, 10s client poll)
GET       /github/status                 read    bulk latest-run per link (20s server cache)
POST      /github/webhook                public  HMAC X-Hub-Signature-256, installation lifecycle
```

Link PUT is all-or-nothing: mint project-scoped deploy token (hash-only) → PUT secrets
(sealed box) → PUT workflow file (sha-aware) → upsert row. Any failure rolls the
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
| Private repos | Builds count against GitHub Actions minutes |
| Non-default branch | Workflow written with `?ref=` + dispatch on that branch |
| Compose + Dockerfile both present | Compose preferred; user can force single in the form |
| Same repo linked to two projects | Supported — per-project workflow, secret, and concurrency group |
| Repo renamed/transferred | Not yet handled (planned auto-heal via stored `repo_id`) |

## Code Map

| Area | Files |
|---|---|
| Client | `orchestrator/src/github/{mod,auth,repos,actions,manifest,workflow}.rs` |
| Routes | `orchestrator/src/routes/github/{mod,connect,repos,project_link,runs,webhook}.rs` |
| Migrations | `orchestrator/src/db/migrations/0032_github_connect.sql`, `0033_multi_app_connections.sql` |
| Dashboard | `GitHubPanel.tsx`, `GithubCallbackPage.tsx`, `GitHubRepoBrowser.tsx`, `GitHubDeployForm.tsx`, `ProjectCard/GitHubBadge.tsx`, `ProjectCard/GitHubRunsModal.tsx`, `hooks/useGithubStatus.ts` |
| Action | [mtsandeep/l8bin-action](https://github.com/mtsandeep/l8bin-action) v0.2.0 |
