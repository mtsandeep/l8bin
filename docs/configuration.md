# Configuration

All configuration is done via environment variables in `.env` after install.

## Master (Orchestrator)

| Variable | Default | Description |
|---|---|---|
| `DOMAIN` | *(required on Linux)* | Your server domain (e.g. `example.com`) |
| `DASHBOARD_SUBDOMAIN` | `l8bin` | Dashboard served at `{subdomain}.{domain}` |
| `POKE_SUBDOMAIN` | `poke` | Wake-report endpoint subdomain |
| `CADDY_ADMIN_URL` | `http://caddy:2019` | Caddy admin API URL |
| `DATABASE_URL` | `sqlite:./data/litebin.db` | SQLite database path |
| `DOCKER_NETWORK` | `litebin-network` | Docker bridge network shared by all services and app containers |
| `HOST` | `0.0.0.0` | Orchestrator bind address |
| `PORT` | `5080` | Orchestrator API port |
| `DEFAULT_AUTO_STOP_MINS` | `15` | Minutes before idle apps are stopped |
| `JANITOR_INTERVAL_SECS` | `300` | Janitor check interval |
| `HEARTBEAT_INTERVAL_SECS` | `30` | Node health check interval |
| `PUBLIC_IP` | *(auto-detected)* | Override if auto-detection fails (e.g. behind NAT) |
| `ROUTING_MODE` | `master_proxy` | `master_proxy` or `cloudflare_dns` |

## Multi-node mTLS (Master side)

Only needed when connecting worker nodes.

| Variable | Description |
|---|---|
| `MASTER_CA_CERT_PATH` | Root CA certificate path |
| `MASTER_CLIENT_CERT_PATH` | Client certificate for mTLS connections to agents |
| `MASTER_CLIENT_KEY_PATH` | Client key for mTLS connections to agents |

## Cloudflare DNS

Only needed when `ROUTING_MODE=cloudflare_dns`. Also configurable via Dashboard -> Settings.

| Variable | Description |
|---|---|
| `CLOUDFLARE_API_TOKEN` | Cloudflare API token with DNS edit permissions |
| `CLOUDFLARE_ZONE_ID` | Cloudflare zone ID for your domain |

## Agent (Worker node)

All agent variables use the `AGENT_` prefix. Not needed on the master.

| Variable | Default | Description |
|---|---|---|
| `AGENT_CERT_PATH` | *(required)* | Agent's server certificate for mTLS |
| `AGENT_KEY_PATH` | *(required)* | Agent's server key for mTLS |
| `AGENT_CA_CERT_PATH` | *(required)* | CA cert to verify orchestrator |
| `AGENT_PUBLIC_IP` | *(auto-detected)* | Override if behind NAT |
| `AGENT_CADDY_ADMIN_URL` | `http://localhost:2019` | Local Caddy admin API URL |

## CLI

The CLI (`l8b`) uses these environment variables. Set them directly or use `l8b config set`.

| Variable | Description |
|---|---|
| `L8B_SERVER` | LiteBin server URL |
| `L8B_TOKEN` | Deploy token for CI/CD |

### Config file locations

| Platform | Path |
|---|---|
| Linux | `~/.config/litebin/config.toml` |
| macOS | `~/Library/Application Support/litebin/config.toml` |
| Windows | `%APPDATA%\litebin\config.toml` |

### Config priority (highest first)

1. CLI flags (`--server`, `--token`)
2. Environment variables (`L8B_SERVER`, `L8B_TOKEN`)
3. Config file (`config.toml`)
4. Saved session (`session.json`)
