# Command-Line Help for `l8b`

This document contains the help content for the `l8b` command-line program.

**Command Overview:**

* [`l8b`↴](#l8b)
* [`l8b deploy`↴](#l8b-deploy)
* [`l8b ship`↴](#l8b-ship)
* [`l8b list`↴](#l8b-list)
* [`l8b logs`↴](#l8b-logs)
* [`l8b url`↴](#l8b-url)
* [`l8b stop`↴](#l8b-stop)
* [`l8b start`↴](#l8b-start)
* [`l8b restart`↴](#l8b-restart)
* [`l8b delete`↴](#l8b-delete)
* [`l8b init`↴](#l8b-init)
* [`l8b doctor`↴](#l8b-doctor)
* [`l8b mcp`↴](#l8b-mcp)
* [`l8b setup`↴](#l8b-setup)
* [`l8b domain`↴](#l8b-domain)
* [`l8b domain set`↴](#l8b-domain-set)
* [`l8b domain remove`↴](#l8b-domain-remove)
* [`l8b platform`↴](#l8b-platform)
* [`l8b platform domain`↴](#l8b-platform-domain)
* [`l8b platform domain set`↴](#l8b-platform-domain-set)
* [`l8b platform domain show`↴](#l8b-platform-domain-show)
* [`l8b backup`↴](#l8b-backup)
* [`l8b backup status`↴](#l8b-backup-status)
* [`l8b backup enable`↴](#l8b-backup-enable)
* [`l8b backup disable`↴](#l8b-backup-disable)
* [`l8b backup add-s3`↴](#l8b-backup-add-s3)
* [`l8b backup remove-s3`↴](#l8b-backup-remove-s3)
* [`l8b backup restore`↴](#l8b-backup-restore)
* [`l8b backup revert`↴](#l8b-backup-revert)
* [`l8b login`↴](#l8b-login)
* [`l8b logout`↴](#l8b-logout)
* [`l8b status`↴](#l8b-status)
* [`l8b cleanup`↴](#l8b-cleanup)
* [`l8b env`↴](#l8b-env)
* [`l8b env list`↴](#l8b-env-list)
* [`l8b env push`↴](#l8b-env-push)
* [`l8b config`↴](#l8b-config)
* [`l8b config set`↴](#l8b-config-set)
* [`l8b config show`↴](#l8b-config-show)

## `l8b`

LiteBin CLI — deploy apps from your terminal

**Usage:** `l8b [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `deploy` — Deploy the current directory to LiteBin
* `ship` — Interactive deploy — guided flow for new or existing projects
* `list` — List all projects with live status
* `logs` — Show container logs for a project
* `url` — Print the project's managed URL
* `stop` — Stop a running project
* `start` — Start a stopped project
* `restart` — Recreate containers (applies pending .env changes)
* `delete` — Delete a project, its containers, and its volumes
* `init` — Write l8b.toml project defaults (and optionally the workspace .mcp.json)
* `doctor` — Environment sanity checks with recovery hints
* `mcp` — Run as a stdio MCP server exposing l8b as tools (for coding agents)
* `setup` — First-run bootstrap: create the admin account and pair this machine
* `domain` — Manage a project's custom domain
* `platform` — Manage the server's platform domain (admin)
* `backup` — Manage platform database backup (admin)
* `login` — Log in to a LiteBin server (dashboard approval or username/password)
* `logout` — Log out (remove a stored login; default: all of them)
* `status` — Show CLI status and server info
* `cleanup` — Clean up leftover build artifacts (temp generated dockerignore files)
* `env` — Manage runtime environment variables (values are write-only)
* `config` — Manage CLI configuration

###### **Options:**

* `--server <SERVER>` — Server URL (default: from env L8B_SERVER or stored config)
* `--token <TOKEN>` — Deploy token (default: from env L8B_TOKEN or stored config)
* `--ci` — CI mode: suppress verbose output and hide secrets (or set L8B_CI=true)
* `--json` — Machine-readable JSON output (single object on stdout; or set L8B_JSON=1)



## `l8b deploy`

Deploy the current directory to LiteBin

**Usage:** `l8b deploy [OPTIONS]`

###### **Options:**

* `--project <PROJECT>` — Project ID (default: `project` from l8b.toml)
* `--port <PORT>` — Internal app port (first deploy only; redeploys keep the existing port)
* `--background` — Run as a background project with no managed HTTP URL
* `--path <PATH>` — Path to project directory (default: current dir)

  Default value: `.`
* `--node <NODE>` — Target node ID (optional)
* `--dockerfile <DOCKERFILE>` — Dockerfile path (auto-detected if not specified)
* `--cmd <CMD>` — Custom command to run in the container
* `--memory <MEMORY>` — Memory limit in MB
* `--cpu <CPU>` — CPU limit (0.0 - 1.0)
* `--no-auto-stop` — Disable auto-stop
* `--secret <SECRET>` — Include this .env file in the build (repeatable; overrides .env auto-discovery)
* `--no-env` — Exclude build-time env files entirely (skip .env auto-discovery)
* `--env-file <ENV_FILE>` — Push this file as runtime env after deploying (KEY=VALUE lines, merged)
* `--compose` — Force compose mode (auto-detected if a compose file exists)
* `--service <SERVICE>` — Deploy only specific services (repeatable, compose mode only)
* `--grant-capability <GRANT_CAPABILITY>` — Grant a project capability for this deploy (repeatable: docker-observe, host-network, raw-ports)
* `--upload <UPLOAD>` — How to upload the image to the node: auto (default, direct when the node supports it), direct (client → agent, skipping the master relay), or relay (client → master → agent)

  Default value: `auto`

  Possible values:
  - `auto`:
    Let the broker decide (direct when the node supports it, else relay/local)
  - `direct`
  - `relay`




## `l8b ship`

Interactive deploy — guided flow for new or existing projects

**Usage:** `l8b ship [OPTIONS]`

###### **Options:**

* `--path <PATH>` — Path to project directory (default: current dir)

  Default value: `.`
* `--port <PORT>` — App port (default: 3000)
* `--secret <SECRET>` — Include this .env file in the build (repeatable; overrides .env auto-discovery)
* `--no-env` — Exclude build-time env files entirely (skip .env auto-discovery)



## `l8b list`

List all projects with live status

**Usage:** `l8b list`



## `l8b logs`

Show container logs for a project

**Usage:** `l8b logs [OPTIONS] [PROJECT]`

###### **Arguments:**

* `<PROJECT>` — Project ID (default: l8b.toml `project`)

###### **Options:**

* `--tail <TAIL>` — Number of lines to show

  Default value: `100`
* `--service <SERVICE>` — Service name (multi-service projects; defaults to the public service)
* `--deploy` — Show deploy logs instead of container logs



## `l8b url`

Print the project's managed URL

**Usage:** `l8b url [PROJECT]`

###### **Arguments:**

* `<PROJECT>` — Project ID (default: l8b.toml `project`)



## `l8b stop`

Stop a running project

**Usage:** `l8b stop [PROJECT]`

###### **Arguments:**

* `<PROJECT>` — Project ID (default: l8b.toml `project`)



## `l8b start`

Start a stopped project

**Usage:** `l8b start [PROJECT]`

###### **Arguments:**

* `<PROJECT>` — Project ID (default: l8b.toml `project`)



## `l8b restart`

Recreate containers (applies pending .env changes)

**Usage:** `l8b restart [PROJECT]`

###### **Arguments:**

* `<PROJECT>` — Project ID (default: l8b.toml `project`)



## `l8b delete`

Delete a project, its containers, and its volumes

**Usage:** `l8b delete [OPTIONS] [PROJECT]`

###### **Arguments:**

* `<PROJECT>` — Project ID (default: l8b.toml `project`)

###### **Options:**

* `--yes` — Skip the confirmation prompt (required in CI/JSON mode)



## `l8b init`

Write l8b.toml project defaults (and optionally the workspace .mcp.json)

**Usage:** `l8b init [OPTIONS]`

###### **Options:**

* `--project <PROJECT>` — Project ID (used as subdomain)
* `--node <NODE>` — Target node ID
* `--server <SERVER>` — LiteBin server this repo deploys to (default: your only login)
* `--mcp` — Also write a workspace .mcp.json for the litebin MCP server
* `--agents` — Also add (or refresh) a LiteBin section in AGENTS.md
* `--force` — Overwrite an existing l8b.toml / .mcp.json
* `--path <PATH>` — Project directory (default: current dir)

  Default value: `.`



## `l8b doctor`

Environment sanity checks with recovery hints

**Usage:** `l8b doctor`



## `l8b mcp`

Run as a stdio MCP server exposing l8b as tools (for coding agents)

**Usage:** `l8b mcp`



## `l8b setup`

First-run bootstrap: create the admin account and pair this machine

**Usage:** `l8b setup --server <SERVER>`

###### **Options:**

* `--server <SERVER>` — Server URL



## `l8b domain`

Manage a project's custom domain

**Usage:** `l8b domain <COMMAND>`

###### **Subcommands:**

* `set` — Set a custom domain
* `remove` — Clear the custom domain



## `l8b domain set`

Set a custom domain

**Usage:** `l8b domain set [OPTIONS] <DOMAIN>`

###### **Arguments:**

* `<DOMAIN>` — Custom domain (e.g. myapp.example.com)

###### **Options:**

* `-p`, `--project <PROJECT>` — Project ID (default: l8b.toml `project`)



## `l8b domain remove`

Clear the custom domain

**Usage:** `l8b domain remove [OPTIONS]`

###### **Options:**

* `-p`, `--project <PROJECT>` — Project ID (default: l8b.toml `project`)



## `l8b platform`

Manage the server's platform domain (admin)

**Usage:** `l8b platform <COMMAND>`

###### **Subcommands:**

* `domain` — Change or show the platform domain (`{project}.{domain}`)



## `l8b platform domain`

Change or show the platform domain (`{project}.{domain}`)

**Usage:** `l8b platform domain <COMMAND>`

###### **Subcommands:**

* `set` — Change the platform domain (old URLs stop working; you sign in again on the new host)
* `show` — Show the current platform domain and dashboard URL



## `l8b platform domain set`

Change the platform domain (old URLs stop working; you sign in again on the new host)

**Usage:** `l8b platform domain set [OPTIONS] <DOMAIN>`

###### **Arguments:**

* `<DOMAIN>` — New platform domain (e.g. example.com or apps.example.com)

###### **Options:**

* `--ack-dns` — Accept preflight DNS warnings without prompting (required in CI/JSON mode)
* `--timeout <TIMEOUT>` — How long to wait for the change job, in seconds (default 300)



## `l8b platform domain show`

Show the current platform domain and dashboard URL

**Usage:** `l8b platform domain show`



## `l8b backup`

Manage platform database backup (admin)

**Usage:** `l8b backup <COMMAND>`

###### **Subcommands:**

* `status` — Show backup status (enabled, replicas, last sync)
* `enable` — Enable continuous backup (starts the Litestream sidecar)
* `disable` — Disable backup (stops the sidecar; replicas are kept)
* `add-s3` — Add or update an S3/R2 replica
* `remove-s3` — Remove the S3/R2 replica (local backup keeps running)
* `restore` — Restore the database from backup (orchestrator restarts to apply)
* `revert` — Undo the last applied restore (orchestrator restarts to apply)



## `l8b backup status`

Show backup status (enabled, replicas, last sync)

**Usage:** `l8b backup status`



## `l8b backup enable`

Enable continuous backup (starts the Litestream sidecar)

**Usage:** `l8b backup enable`



## `l8b backup disable`

Disable backup (stops the sidecar; replicas are kept)

**Usage:** `l8b backup disable [OPTIONS]`

###### **Options:**

* `--yes` — Skip confirmation (required in CI/JSON mode)



## `l8b backup add-s3`

Add or update an S3/R2 replica

**Usage:** `l8b backup add-s3 [OPTIONS] --endpoint <ENDPOINT> --bucket <BUCKET> --access-key <ACCESS_KEY> --secret-key <SECRET_KEY>`

###### **Options:**

* `--endpoint <ENDPOINT>` — S3 endpoint (e.g. https://<account_id>.r2.cloudflarestorage.com)
* `--yes` — Proceed even if the destination already holds backup files (required in CI/JSON mode)
* `--bucket <BUCKET>` — Bucket name
* `--prefix <PREFIX>` — Key prefix (default litebin/)
* `--access-key <ACCESS_KEY>` — Access key ID
* `--secret-key <SECRET_KEY>` — Secret access key



## `l8b backup remove-s3`

Remove the S3/R2 replica (local backup keeps running)

**Usage:** `l8b backup remove-s3 [OPTIONS]`

###### **Options:**

* `--yes` — Skip confirmation (required in CI/JSON mode)



## `l8b backup restore`

Restore the database from backup (orchestrator restarts to apply)

**Usage:** `l8b backup restore [OPTIONS]`

###### **Options:**

* `--timestamp <TIMESTAMP>` — Point-in-time to restore to (RFC 3339, e.g. 2026-10-06T10:30:00Z)
* `--from-local <FROM_LOCAL>` — Restore a local daily snapshot by date (YYYY-MM-DD) instead — bypasses R2/S3
* `--point <POINT>` — Restore a saved point by id (see `l8b backup status`) instead
* `--yes` — Skip confirmation (required in CI/JSON mode)



## `l8b backup revert`

Undo the last applied restore (orchestrator restarts to apply)

**Usage:** `l8b backup revert [OPTIONS]`

###### **Options:**

* `--yes` — Skip confirmation (required in CI/JSON mode)



## `l8b login`

Log in to a LiteBin server (dashboard approval or username/password)

**Usage:** `l8b login [OPTIONS] --server <SERVER>`

###### **Options:**

* `--server <SERVER>` — Server URL
* `--scope <SCOPE>` — Scope to request: read, deploy, manage (default), or admin — the approver decides
* `--pair` — Skip the menu and pair via dashboard approval
* `--password` — Skip the menu and log in with username/password (session)



## `l8b logout`

Log out (remove a stored login; default: all of them)

**Usage:** `l8b logout [OPTIONS]`

###### **Options:**

* `--server <SERVER>` — Log out of this server only



## `l8b status`

Show CLI status and server info

**Usage:** `l8b status [OPTIONS]`

###### **Options:**

* `-p`, `--project <PROJECT>` — Show status of a specific project
* `--wait` — Wait until the project reaches a terminal state; exit 0 only if running
* `--timeout <TIMEOUT>` — Wait timeout in seconds (default 120, with --wait)
* `--healthy` — Probe the project URL for a 2xx (implies --wait; skipped for background projects)



## `l8b cleanup`

Clean up leftover build artifacts (temp generated dockerignore files)

**Usage:** `l8b cleanup [PATH]`

###### **Arguments:**

* `<PATH>` — Project directory (default: current directory)

  Default value: `.`



## `l8b env`

Manage runtime environment variables (values are write-only)

**Usage:** `l8b env <COMMAND>`

###### **Subcommands:**

* `list` — List env keys with masked previews (never values)
* `push` — Push env vars from a file or stdin (never from arguments)



## `l8b env list`

List env keys with masked previews (never values)

**Usage:** `l8b env list [PROJECT]`

###### **Arguments:**

* `<PROJECT>` — Project ID (default: l8b.toml `project`)



## `l8b env push`

Push env vars from a file or stdin (never from arguments)

**Usage:** `l8b env push [OPTIONS] [PROJECT]`

###### **Arguments:**

* `<PROJECT>` — Project ID (default: l8b.toml `project`)

###### **Options:**

* `--file <FILE>` — Env file to push (default: .env in the current directory)
* `--stdin` — Read env content from stdin instead of a file (e.g. `sops -d … | l8b env push --stdin`)
* `--replace` — Replace the whole env file instead of merging into it
* `--apply` — Recreate the container after pushing to apply changes



## `l8b config`

Manage CLI configuration

**Usage:** `l8b config <COMMAND>`

###### **Subcommands:**

* `set` — Set configuration values
* `show` — Show current configuration



## `l8b config set`

Set configuration values

**Usage:** `l8b config set [OPTIONS]`

###### **Options:**

* `--server <SERVER>` — Server URL
* `--token <TOKEN>` — Deploy token



## `l8b config show`

Show current configuration

**Usage:** `l8b config show`



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>

