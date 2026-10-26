# Registries

LiteBin can pull images from any registry — public ones work with zero configuration. Registry entries add **credentials** and **mirrors** so private and rate-limited registries work the same way, on every node, without configuring anything on the nodes themselves.

Registries are managed from **Settings → Registries** (or the `/registries` API).

## Built-ins

Two registries come preconfigured:

| Name | URL | Default | Mirrors |
|---|---|---|---|
| Docker Hub | `https://index.docker.io/v1/` | ✓ | none — one click to add Google's `mirror.gcr.io` |
| GitHub Container Registry | `https://ghcr.io` | — | — |

Built-ins can be edited (credentials, mirrors, default flag) but not deleted. Every other registry pulls anonymously by default — add a row only when you need credentials or mirrors for it.

## Default registry & mirrors

The **default registry** (star in the dashboard) serves image names that carry no registry host: `nginx:alpine` and `myorg/app` resolve through it. Docker Hub ships as the default; flagging another registry moves the choice to you. One caveat worth knowing: bare names like `nginx` mean Docker Hub's `library/*` official images, so a non-Docker-Hub default only works if it mirrors Docker Hub's image paths.

**Mirrors** are pull-through caches of a registry, tried in your chosen order *first*; the registry itself is always the **final fallback** — a failing mirror just falls through, so a dead mirror can never break a deploy. This is the same ordering Docker Engine and podman use for mirroring, and it's what makes a mirror useful: configured = used, so origin traffic (and Docker Hub rate-limit hits) drops to ~zero. A mirror must serve the same repository paths as its upstream (any real pull-through cache does: `mirror.gcr.io`, zot mirroring, Harbor proxy cache).

**Each mirror has its own optional credentials** — a private mirror (your zot with htpasswd) can front a public registry (Docker Hub). Mirror credentials never apply to the registry, and registry credentials never apply to mirrors.

No mirror is configured out of the box — routing your image traffic through a third party is your call. When it's the right call, the Docker Hub entry has a one-click **"+ add Google's mirror"** action (Google's `mirror.gcr.io`, anonymous, serves Docker Hub's image paths): un-prefixed pulls then avoid Docker Hub rate limits entirely. Removing or replacing it later is one dashboard edit.

Notes on mirror semantics (shared with Docker's own mirror support):

- Mutable tags can differ across sources: `app:latest` cached on a mirror yesterday may be older than on the registry today. Digest references (`@sha256:…`) are immune.
- The pull log records which source served each image.
- The project stores your original image name; the container runs under the reference it was actually pulled as. Reordering mirrors changes where the *next* pull comes from.
- The mirror chain applies to LiteBin's pulls — `docker build` base images on a build box use the daemon's own mirror config instead.

## Adding a registry

**Settings → Registries → Add registry**: a name, the registry URL (`https://registry.example.com`), and optionally credentials — a **username** and **password/token**, exactly as the provider issues them (PAT as the password for GHCR and GitLab). Encoding is handled internally; the API also accepts a pre-encoded `auth_b64` for scripting.

Works with private registries that accept Basic authentication at the registry or token endpoint: zot (htpasswd), Harbor, GHCR (PAT as password), Docker Hub (paid accounts), GitLab registry, Quay.

Deploy by reference from that point on: an image like `registry.example.com/team/app:1.2` in a single-service deploy or anywhere in a compose file pulls with the configured credentials. A compose file can freely mix registries — credentials are matched per image.

The **Test** button checks the registry:

- **200** — reachable, credentials accepted.
- **Token-auth registries** (Docker Hub, ghcr.io, quay…): LiteBin completes the token exchange the way `docker login` does and reports whether the credentials were verified. Without stored credentials, a reachable registry reports OK with a note that public images pull anonymously while private ones need credentials — public/private is per-package, not per-registry.
- **Basic-auth registries** (e.g. zot with htpasswd): 401 means the stored credentials were rejected, or none are stored.
- **Unreachable** — wrong URL or network problem; the message includes the failing URL.

## How pulls resolve

Docker only applies registry credentials when they match the registry being pulled, so LiteBin resolves **per image reference**: the registry host is taken from the image name (falling back to the default registry for host-less names), canonicalized (`index.docker.io` = `registry-1.docker.io` = `docker.io`), and matched against the configured registries. The pull then tries the matched registry's mirrors in order, then the registry itself — first success wins, and the deploy log says which source served. Unknown registries (no row) pull anonymously and directly. Images uploaded by `l8b deploy` (`sha256:` references) never touch a registry at all.

- **Master node** pulls use the configured state directly; changes take effect on the next pull.
- **Agent nodes** receive the full registry state (default, credentials, mirrors) inside the deploy request itself, over the existing mTLS channel. Nothing is configured or stored on the agent — it lives in memory for the pulls only. New nodes need no registry setup.

Nodes are expected to run the same LiteBin version as the orchestrator. An out-of-date agent ignores the registry config field, so a private-registry deploy fails with a pull error until the agent is upgraded.

## API (session)

```
GET    /registries            → list; credentials masked (has_auth + last characters)
POST   /registries            → { name, url, auth_b64?, mirrors?, is_default? }
PUT    /registries/{id}       → { name?, url?, auth_b64?, clear_auth?, mirrors?, is_default? } — omitted fields are kept
DELETE /registries/{id}       → built-ins are rejected
POST   /registries/{id}/test  → { status: ok|unauthorized|unexpected_status|unreachable, http_status?, message }
```

Setting `is_default: true` unsets the previous default. Setting `is_default: false` leaves no default — bare image names then use the daemon's own docker.io resolution.

## Security notes

- Credentials are stored in the platform SQLite database in plaintext — the same trust level as the Cloudflare API token in Settings. `GET` responses never include the full value; the dashboard only ever shows it after you type it.
- Credentials travel to agents only inside mTLS-encrypted deploy requests and are never persisted agent-side.
- Registries require a dashboard session — deploy tokens cannot read or change them.

## Migrating from the old environment variables

`LITEBIN_REGISTRY_URL` / `LITEBIN_REGISTRY_AUTH` (and the `~/.docker/config.json` fallback) are gone. If you used them, add the registry once in Settings → Registries and remove the variables — behavior is otherwise identical, plus multi-registry and agent distribution now work.
