# The `l8b ship` flow

`l8b ship` is the one command that runs everything: pick a destination, build and deploy, push env, grant capabilities, start, stop, come back tomorrow and resume — all guided, all from the project directory. `l8b deploy` is its non-interactive twin for CI — same destinations, same env handling, no prompts (see the [CLI reference](../reference/cli.md)).

```bash
l8b ship                 # from the project directory
l8b ship --path ../api   # or point elsewhere
```

## Where it deploys: anchor and destinations

Two files decide the default target. Both use the same format; values in the local file win.

| File | Committed? | Holds |
|---|---|---|
| `l8b.toml` | Your call — commit it when teammates/agents should inherit the target, gitignore it for personal or demo repos | The anchor (`project`/`server`/`node`) and any shared destinations |
| `l8b.local.toml` | Never — it is personal | Named destinations and the `active` pointer |

A destination is a named server + project pair:

```toml
active = "sandbox"

[destinations.sandbox]
server = "http://localhost:5080"
project = "myapp-sandbox"

[destinations.prod]
server = "https://dash.example.com"
project = "myapp"

# Optional — pin to a node when the server runs several (e.g. `local`, an agent id)
[destinations.edge]
server = "https://dash.example.com"
project = "myapp-edge"
node = "eu-agent-1"
```

`l8b.local.toml` appears on its own the first time you save a destination — from `l8b dest add`, from the ship menu's *Add a destination…*, from the save prompt after deploying somewhere new, or from `l8b dest use`. Plain deploys to the anchor never create it. Neither file holds secrets; runtime env lives on the server (`l8b env push`).

### Choosing at ship time

- **No destinations** — the anchor (or stored login) is used, as before.
- **One destination** — used silently.
- **Several** — ship lists them (cursor on `active`), plus *Add a destination…* and *Other server (not saved)…*:

```
? Deploy to: › sandbox  http://localhost:5080  myapp-sandbox
              prod     https://dash.example.com  myapp
              Add a destination…
              Other server (not saved)…
```

Picking one moves `active`, so the next `l8b deploy`, `l8b status`, or `l8b env` command follows it. Deploying to an unsaved server ends with:

```
Save 'https://staging.example.com' as a destination for this repo? (Y/n)
Destination name: [staging-example-com]
  ✓ Saved destination 'staging-example-com' → l8b.local.toml (local, not committed)
```

Manage the list with `l8b dest list | add | use | rm` — `list` shows each destination's origin (shared or local) and which is active.

## Variant stacks: picking the compose file

A repo may carry more than one stack — `docker-compose.yml` and `docker-compose.staging.yml` (a smaller variant), say. When `ship` finds several compose files it asks which to deploy and remembers the choice (`compose` key in `l8b.local.toml`):

```
? Compose file: › docker-compose.yml
                 docker-compose.staging.yml
```

The resolution order everywhere: `--compose <file>` flag > the remembered choice > the first well-known name (`compose.yaml`, `docker-compose.yml`, …). Override files (`docker-compose.override.*`) never appear in the picker — but they can be *used* explicitly: several files merge in docker's order (mappings deep-merge, later files win on scalars and replace lists):

```bash
l8b deploy --compose docker-compose.yml --compose docker-compose.override.yml
```

```toml
# l8b.toml / l8b.local.toml — single file or a merged list
compose = ["docker-compose.yml", "docker-compose.prod.yml"]
```

> **Note — merge coverage.** LiteBin merges files itself (the Docker daemon has no compose knowledge): mappings deep-merge, later files win on scalars and **replace** lists. This covers the common override patterns, but is not the full `docker compose` spec — key-based list merges (`environment`/`labels` in list form, `volumes` merged by target, `ports` unions) and the `!reset`/`!override` tags or `include:` directive are not supported yet. Redeclare such fields fully in the later file.

Switching a project between stacks is a topology change: delete the project and deploy fresh with the other file.

## The flow, stage by stage

After the target is known, ship walks the same stages every time.

**1. Project menu** — the repo's default project is preselected (`myapp (from l8b.toml)`); *New project* / *Existing project* cover the rest. For an existing project: *Redeploy*, *Recreate*, *Start*/*Stop*, *Delete* (and *Resume deployment* while a first deploy is paused).

**2. Build-time env** — when `.env*` files are discovered, ship confirms which are **baked into the image** (`.env` wins over `.env.example`, `.local`/`.prod` rank higher). Flags exist for scripts: `--secret <file>` (repeatable) picks exactly, `--no-env` skips. This is separate from runtime env — see [env-secrets](env-secrets.md).

**3. Runtime env** — variables the **containers** need at start (passwords, API keys; compose `${JWT_SECRET:?…}` and friends).

- *First deploy*: the deploy **stages** (images + compose stored, nothing starts) and pauses at the runtime-config checkpoint — push a local `.env`, edit the file on the node, or skip; then *Start containers now*.
- *Redeploy / Recreate*: ship asks **before** submitting, because the deploy auto-starts — pushing after would miss the first start:

```
  🔒 Build-time env: › Use these files
  🔒 Runtime env: › Skip, no changes to env / Push from a local .env file
```

**4. Capabilities** — a compose that needs raw-ports, docker-observe or host-network prompts *How should the deploy proceed?* (grant / strip host ports / defer / abort). Details in the [FAQ](faq.md#why-is-my-deploy-asking-for-raw-ports-host-ports).

**5. Build, upload, start** — services build (identical contexts share one image), upload, then: first deploy waits for your *Start containers now*; a redeploy auto-starts and polls to `running`. One-shot services (migrations, seeders) run in dependency order — see [multi-service](multi-service.md#dependency-conditions).

## Credentials

Ship talks to one server per run. When stored credentials are rejected (expired token, revoked session), it offers to fix it inline:

```
? Credentials rejected: › Re-pair with http://localhost:5080
                         https://dash.example.com
                         Add a new server…
                         Cancel
```

Re-pairing runs the login flow and retries automatically; switching rebinds the run. Pair a server ahead of time with `l8b setup --server <url>`.

## Troubleshooting

| Symptom | Meaning / fix |
|---|---|
| `multiple destinations configured (a, b) with none active` | Non-interactive commands refuse to guess — `l8b dest use <name>`, or set `active` in `l8b.local.toml`. `ship` will prompt instead. |
| `l8b.toml names project 'x' but it is not on this server` | The active destination points elsewhere — check `l8b dest list`. |
| `Deploy failed … variable interpolation error: X must be set` | The compose requires a runtime variable — answer the runtime-env prompt with a push (or `l8b env push`). |
| Wrote `l8b.toml` after deploying to a new server | That only happens while no destinations exist; with destinations defined, one-off targets are offered as saves instead. |
