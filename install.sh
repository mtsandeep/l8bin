#!/usr/bin/env bash
set -euo pipefail

REPO="mtsandeep/l8bin"
L8B_IN="${L8B_IN:-https://l8b.in}"
CHANGELOG_URL="${CHANGELOG_URL:-https://github.com/${REPO}/releases}"

# -- Colors ------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
PURPLE='\033[0;35m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

info()  { echo -e "${GREEN}==> ${NC}$1"; }
warn()  { echo -e "${YELLOW}==> ${NC}$1"; }
error() { echo -e "${RED}Error:${NC} $1" >&2; }
die()   { error "$1"; exit 1; }

_tty_read() {
  if [ ! -t 0 ] && [ -t 1 ]; then
    read -r "$@" < /dev/tty
  else
    read -r "$@"
  fi
}

prompt() {
  local msg="$1" var="$2" default="${3:-}"
  if [ -n "$default" ]; then
    echo -ne "${CYAN}${msg}${NC} [${default}]: "
  else
    echo -ne "${CYAN}${msg}${NC}: "
  fi
  local value
  _tty_read value
  eval "$var=\"${value:-$default}\""
}

prompt_yes() {
  local msg="$1" default="${2:-n}"
  local yn
  echo -ne "${CYAN}${msg}${NC} [${default}]: "
  _tty_read yn
  yn="${yn:-$default}"
  [[ "$yn" =~ ^[Yy]$ ]]
}

# -- Platform detection -----------------------------------------------------
detect_platform() {
  case "$(uname -s)" in
    Linux)            echo "linux" ;;
    Darwin)           echo "macos" ;;
    MINGW*|MSYS*|CYGWIN*) echo "windows" ;;
    *)                die "Unsupported OS: $(uname -s)" ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    aarch64|arm64) echo "aarch64" ;;
    *) die "Unsupported architecture: $(uname -m)" ;;
  esac
}

is_windows() { [ "$(detect_platform)" = "windows" ]; }
is_linux()   { [ "$(detect_platform)" = "linux" ]; }

# -- Helpers -----------------------------------------------------------------
require_cmd() {
  command -v "$1" &>/dev/null || die "'$1' is required but not installed"
}

ensure_docker() {
  if command -v docker &>/dev/null; then
    info "Docker found: $(docker --version)"
  else
    die "Docker is required. Install Docker: https://docs.docker.com/get-docker/"
  fi
}

ensure_docker_compose() {
  if docker compose version &>/dev/null 2>&1; then
    info "Docker Compose available: $(docker compose version --short 2>/dev/null || echo 'ok')"
  else
    die "Docker Compose is not available. Install Docker Compose plugin: https://docs.docker.com/compose/install/"
  fi
}

get_latest_release() {
  if [ -n "${L8B_RELEASE_DIR:-}" ]; then
    echo "local"
    return
  fi
  local response
  response=$(curl -sf "https://api.github.com/repos/${REPO}/releases/latest" 2>&1) || true
  if [ -z "$response" ]; then
    die "Could not fetch latest release from GitHub. Check that https://github.com/${REPO}/releases exists and has at least one release."
  fi
  local tag
  if command -v jq &>/dev/null; then
    tag=$(echo "$response" | jq -r '.tag_name')
  else
    # Fallback: grep the tag_name from JSON
    tag=$(echo "$response" | grep -o '"tag_name": *"[^"]*"' | head -1 | cut -d'"' -f4)
  fi
  if [ -z "$tag" ] || [ "$tag" = "null" ]; then
    die "No releases found at https://github.com/${REPO}/releases. At least one release must be published before running the installer."
  fi
  echo "$tag"
}

download_and_verify() {
  local url="$1" dest="$2" name="$3"

  if [ -n "${L8B_RELEASE_DIR:-}" ]; then
    local src="${L8B_RELEASE_DIR}/$(basename "$url")"
    [ -f "$src" ] || die "Local release file not found: $src"
    info "Using local ${name}..."
    cp "$src" "$dest"
    return
  fi

  info "Downloading ${name}..."
  curl -sfL "$url" -o "$dest"

  local checksum_cmd="sha256sum"
  command -v sha256sum &>/dev/null || checksum_cmd="shasum -a 256"

  if command -v $checksum_cmd &>/dev/null; then
    local checksums
    checksums=$(curl -sfL "${url%/*}/checksums.txt")
    local expected
    expected=$(echo "$checksums" | grep "$(basename "$url")" | cut -d' ' -f1)
    if [ -n "$expected" ]; then
      local actual
      actual=$($checksum_cmd "$dest" | cut -d' ' -f1)
      if [ "$expected" != "$actual" ]; then
        die "Checksum mismatch for ${name}! Expected: ${expected}, Got: ${actual}"
      fi
      info "Checksum verified."
    fi
  fi
}

base64_encode() {
  base64 -w0 2>/dev/null || base64 | tr -d '\r\n'
}

base64_decode() {
  base64 -d 2>/dev/null || base64 --decode
}

unpack_cert_bundle() {
  local dir="$1"
  awk -v dir="$dir" '
    /-----BEGIN CERTIFICATE-----/ { count++; if (count==1) file="ca.pem"; else file="agent.pem" }
    /-----BEGIN.*PRIVATE KEY-----/ { file="agent-key.pem" }
    file { print > dir"/"file }
  '
}

configure_ufw() {
  if command -v ufw &>/dev/null; then
    info "Opening required ports..."
    ufw allow 80/tcp
    ufw allow 443/tcp
    ufw allow 443/udp
    ufw allow 5083/tcp
    info "Ports 80, 443, 5083 opened."
  else
    warn "UFW not found. Make sure ports 80, 443, 5083 are open on your firewall."
  fi
}

# -- Path helpers -----------------------------------------------------------
find_install_dir() {
  local not_found_msg="${1:-LiteBin installation not found. Run the setup first.}"
  if [ "$(id -u)" -eq 0 ] && [ -d "/opt/litebin" ]; then
    echo "/opt/litebin"
  elif [ -d "${HOME}/litebin" ]; then
    echo "${HOME}/litebin"
  else
    die "$not_found_msg"
  fi
}

find_certs_dir() {
  local install_dir="$1"
  if [ "$(id -u)" -eq 0 ]; then
    echo "/etc/litebin/certs"
  else
    echo "${install_dir}/certs"
  fi
}

ensure_agent_network() {
  if ! docker network ls --format '{{.Name}}' | grep -q "^litebin-network$"; then
    docker network create litebin-network >/dev/null
  fi
}

run_agent_container() {
  local install_dir="$1" certs_dir="$2" agent_port="$3"
  ensure_agent_network
  docker run -d \
    --name litebin-agent \
    --restart unless-stopped \
    --network litebin-network \
    --env-file "${install_dir}/agent/.env" \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v "${certs_dir}":/certs:ro \
    -v litebin-agent-data:/etc/litebin/data \
    -p "${agent_port}:8443" \
    litebin-agent
}

run_agent_caddy() {
  local install_dir="${1:-}"
  ensure_agent_network
  local -a mounts=(
    -v litebin-agent-caddy-data:/data
    -v litebin-agent-caddy-config:/config
    -v litebin-agent-caddy-root:/root/.local/share/caddy
  )
  [ -n "$install_dir" ] && [ -f "${install_dir}/agent/Caddyfile" ] && mounts+=(-v "${install_dir}/agent/Caddyfile:/etc/caddy/Caddyfile:ro")
  docker stop litebin-agent-caddy 2>/dev/null || true
  docker rm litebin-agent-caddy 2>/dev/null || true
  docker run -d \
    --name litebin-agent-caddy \
    --restart unless-stopped \
    --network litebin-network \
    -p 80:80 \
    -p 443:443 \
    -p 443:443/udp \
    "${mounts[@]}" \
    caddy:2.11.2-alpine
}

generate_compose() {
  local dest="$1"
  local certs_dir="${2:-}"

  local orch_volumes="      - /var/run/docker.sock:/var/run/docker.sock
      - orchestrator-data:/app/data
      - ./projects:/app/projects"
  [ -n "$certs_dir" ] && orch_volumes="${orch_volumes}
      - ${certs_dir}:/certs:ro"

  local caddy_volumes="      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy-data:/data
      - caddy-config:/config
      - caddy-root:/root/.local/share/caddy"
  [ -n "$certs_dir" ] && caddy_volumes="${caddy_volumes}
      - ${certs_dir}:/certs:ro"

  cat > "$dest" <<COMPOSE_EOF
services:
  orchestrator:
    build: ./orchestrator
    container_name: litebin-orchestrator
    restart: unless-stopped
    volumes:
${orch_volumes}
    env_file:
      - .env
    depends_on:
      - caddy
    networks:
      - litebin-network

  dashboard:
    build: ./dashboard
    container_name: litebin-dashboard
    restart: unless-stopped
    networks:
      - litebin-network

  caddy:
    image: caddy:2.11.2-alpine
    container_name: litebin-caddy
    restart: unless-stopped
    env_file:
      - .env
    ports:
      - "80:80"
      - "443:443"
      - "443:443/udp"
    extra_hosts:
      - "host.docker.internal:host-gateway"
    volumes:
${caddy_volumes}
    networks:
      - litebin-network

networks:
  litebin-network:
    name: litebin-network
    driver: bridge

volumes:
  orchestrator-data:
  caddy-data:
  caddy-config:
  caddy-root:
COMPOSE_EOF
}

# -- CLI Install -------------------------------------------------------------
install_cli() {
  local platform arch binary install_name

  platform=$(detect_platform)
  arch=$(detect_arch)

  case "$platform" in
    linux)   binary="l8b-${arch}-linux";    install_name="l8b" ;;
    macos)   binary="l8b-${arch}-macos";    install_name="l8b" ;;
    windows) binary="l8b-${arch}-windows.exe"; install_name="l8b.exe" ;;
  esac

  local release_url
  release_url=$(get_latest_release)
  [ -z "$release_url" ] || [ "$release_url" = "null" ] && die "Failed to detect latest release"

  local install_dir="$HOME/.local/bin"
  mkdir -p "$install_dir"
  local install_path="${install_dir}/${install_name}"

  download_and_verify \
    "https://github.com/${REPO}/releases/download/${release_url}/${binary}" \
    "$install_path" \
    "l8b ${release_url} (${platform}/${arch})"

  chmod +x "$install_path"

  echo ""
  info "Installed l8b ${release_url} to ${install_path}"
  echo ""
  if is_windows; then
    echo "  Add to your PATH:  setx PATH \"%PATH%;${install_dir}\""
  else
    echo "  Add to your PATH:  export PATH=\"${install_dir}:\$PATH\""
  fi
  echo ""
  echo -e "  Then run: ${BOLD}l8b --help${NC}"
}

# -- Master Install ----------------------------------------------------------
install_master() {
  local platform arch

  platform=$(detect_platform)
  arch=$(detect_arch)

  # Determine install paths based on platform
  local install_dir certs_dir
  if is_windows; then
    install_dir="${HOME}/litebin"
    certs_dir="${install_dir}/certs"
  elif [ "$(id -u)" -eq 0 ]; then
    install_dir="/opt/litebin"
    certs_dir="/etc/litebin/certs"
  else
    warn "Not running as root. Installing to ${HOME}/litebin"
    warn "Configure your firewall manually (ports 80, 443, 5083)."
    install_dir="${HOME}/litebin"
    certs_dir="${install_dir}/certs"
  fi

  ensure_docker
  ensure_docker_compose

  # Check for existing installation → redirect to update
  if [ -f "${install_dir}/docker-compose.yml" ]; then
    if docker compose -f "${install_dir}/docker-compose.yml" ps --format '{{.Names}}' 2>/dev/null | grep -q "litebin"; then
      update_master
      exit $?
    fi
  fi

  # Get release
  local release_url
  release_url=$(get_latest_release)
  [ -z "$release_url" ] || [ "$release_url" = "null" ] && die "Failed to detect latest release"

  # Create directories
  mkdir -p "${install_dir}/orchestrator"
  mkdir -p "${install_dir}/dashboard"
  mkdir -p "${install_dir}/projects"
  mkdir -p "$certs_dir"

  # Download orchestrator binary (always Linux — runs inside Docker container)
  download_and_verify \
    "https://github.com/${REPO}/releases/download/${release_url}/litebin-orchestrator-${arch}-linux" \
    "${install_dir}/orchestrator/litebin-orchestrator" \
    "orchestrator (${arch})"
  chmod +x "${install_dir}/orchestrator/litebin-orchestrator"

  # Download dashboard
  if [ -n "${L8B_RELEASE_DIR:-}" ] && [ -d "${L8B_RELEASE_DIR}/l8b-dashboard-dist" ]; then
    # Local mode: copy directory directly (avoids tar format issues)
    info "Using local dashboard..."
    mkdir -p "${install_dir}/dashboard/dist"
    cp -r "${L8B_RELEASE_DIR}/l8b-dashboard-dist/." "${install_dir}/dashboard/dist/"
  else
    local tmp_tar="/tmp/l8b-dashboard.tar.gz"
    download_and_verify \
      "https://github.com/${REPO}/releases/download/${release_url}/l8b-dashboard.tar.gz" \
      "$tmp_tar" \
      "dashboard"
    tar -xzf "$tmp_tar" -C "${install_dir}/dashboard/"
    rm -f "$tmp_tar"
  fi

  # Generate orchestrator Dockerfile
  cat > "${install_dir}/orchestrator/Dockerfile" <<'ORCH_DOCKERFILE'
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY litebin-orchestrator /app/litebin-orchestrator
RUN chmod +x /app/litebin-orchestrator
WORKDIR /app
RUN mkdir -p /app/data
CMD ["/app/litebin-orchestrator"]
ORCH_DOCKERFILE

  # Generate dashboard Dockerfile
  cat > "${install_dir}/dashboard/Dockerfile" <<'DASH_DOCKERFILE'
FROM nginx:alpine
COPY dist/ /usr/share/nginx/html/
EXPOSE 80
DASH_DOCKERFILE

  # -- Interactive prompts ------------------------------------------------
  echo ""
  echo -e "${BOLD}LiteBin Master Setup${NC}"
  echo ""

  local domain_default="localhost"
  if is_linux; then
    domain_default=""
  fi

  prompt "Domain name (e.g. example.com)" DOMAIN "$domain_default"
  [ -z "$DOMAIN" ] && die "Domain is required"

  # Auto-detect public IP for DNS instructions
  PUBLIC_IP=$(curl -sf --connect-timeout 3 --max-time 5 https://api.ipify.org 2>/dev/null \
    || curl -sf --connect-timeout 3 --max-time 5 https://checkip.amazonaws.com 2>/dev/null \
    || curl -sf --connect-timeout 3 --max-time 5 https://ipv4.icanhazip.com 2>/dev/null \
    || echo "")
  PUBLIC_IP=$(echo "$PUBLIC_IP" | tr -d '[:space:]')
  [ -z "$PUBLIC_IP" ] && warn "Could not detect public IP automatically — update it via Dashboard > Settings after setup"

  local dash_default="l8bin"

  prompt "Dashboard subdomain" DASHBOARD_SUBDOMAIN "$dash_default"
  prompt "Poke subdomain" POKE_SUBDOMAIN "poke"

  local routing_mode="master_proxy"
  if [ "$DOMAIN" != "localhost" ]; then
    echo ""
    echo "  Routing mode:"
    echo "    1) master_proxy  (default — all traffic through this server)"
    echo "    2) cloudflare_dns (each app gets its own DNS record via Cloudflare)"
    echo ""
    local mode_choice
    echo -ne "${CYAN}Choose routing mode [1]: ${NC}"
    _tty_read mode_choice
    case "${mode_choice:-1}" in
      2) routing_mode="cloudflare_dns" ;;
      *) routing_mode="master_proxy" ;;
    esac
  fi

  local cf_api_token="" cf_zone_id=""
  if [ "$routing_mode" = "cloudflare_dns" ]; then
    prompt "Cloudflare API token" cf_api_token ""
    prompt "Cloudflare Zone ID" cf_zone_id ""
    [ -z "$cf_api_token" ] && die "Cloudflare API token is required for cloudflare_dns mode"
    [ -z "$cf_zone_id" ] && die "Cloudflare Zone ID is required for cloudflare_dns mode"
  fi

  # -- Generate .env -----------------------------------------------------
  cat > "${install_dir}/.env" <<EOF
# LiteBin Master Configuration
# Generated by install.sh on $(date -u +%Y-%m-%dT%H:%M:%SZ)

# Domain
DOMAIN=${DOMAIN}
DASHBOARD_SUBDOMAIN=${DASHBOARD_SUBDOMAIN}
POKE_SUBDOMAIN=${POKE_SUBDOMAIN}

# Caddy
CADDY_ADMIN_URL=http://caddy:2019

# Database
DATABASE_URL=sqlite:./data/litebin.db

# Docker
DOCKER_NETWORK=litebin-network

# Server
HOST=0.0.0.0
PORT=5080

# Sleep / Janitor
DEFAULT_AUTO_STOP_MINS=15
JANITOR_INTERVAL_SECS=300
FLUSH_INTERVAL_SECS=60

# Routing
ROUTING_MODE=${routing_mode}
EOF

  if [ "$routing_mode" = "cloudflare_dns" ]; then
    cat >> "${install_dir}/.env" <<EOF

# Cloudflare DNS
CLOUDFLARE_API_TOKEN=${cf_api_token}
CLOUDFLARE_ZONE_ID=${cf_zone_id}
EOF
  fi

  chmod 600 "${install_dir}/.env" 2>/dev/null || true

  # -- Generate Caddyfile ------------------------------------------------
  cat > "${install_dir}/Caddyfile" <<'CADDYFILE'
{
	admin 0.0.0.0:2019
}

{$DASHBOARD_SUBDOMAIN}.{$DOMAIN} {
	handle /auth/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /projects {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /projects/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /deploy {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /deploy/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /deploy-tokens {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /deploy-tokens/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /images/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /nodes {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /nodes/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /settings {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /settings/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /health {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /caddy/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /system/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle {
		reverse_proxy litebin-dashboard:80
	}
}
CADDYFILE

  # -- Generate docker-compose.yml --------------------------------------
  generate_compose "${install_dir}/docker-compose.yml"

  # -- Configure firewall (Linux only) ----------------------------------
  if is_linux && [ "$(id -u)" -eq 0 ]; then
    configure_ufw
  fi

  # -- Start ------------------------------------------------------------
  echo ""
  info "Starting LiteBin..."
  (cd "$install_dir" && docker compose up -d --build 2>&1 | tail -5)
  echo ""

  # -- Done -------------------------------------------------------------
  local dashboard_url
  if [ "$DOMAIN" = "localhost" ]; then
    dashboard_url="https://l8bin.localhost"
  else
    dashboard_url="https://${DASHBOARD_SUBDOMAIN}.${DOMAIN}"
  fi

  echo ""
  echo -e "${GREEN}${BOLD}  LiteBin is running!${NC}"
  echo ""
  echo -e "  Dashboard:  ${CYAN}${dashboard_url}${NC}"
  echo -e "  API:        ${CYAN}${dashboard_url}${NC} (proxied via Caddy)"
  echo ""

  if [ "$DOMAIN" != "localhost" ]; then
    local ip_display="${PUBLIC_IP:-<your server IP>}"
    echo "  DNS setup required:"
    if [ "$routing_mode" = "cloudflare_dns" ]; then
      echo "    You chose ${BOLD}cloudflare_dns${NC} mode. Create these DNS records (DNS-only, grey cloud):"
      echo ""
      echo -e "      ${YELLOW}A${NC}  ${DASHBOARD_SUBDOMAIN}.${DOMAIN}  →  ${ip_display}"
      echo -e "      ${YELLOW}A${NC}  ${POKE_SUBDOMAIN}.${DOMAIN}      →  ${ip_display}"
      echo ""
      echo "    All app subdomains are managed automatically via the Cloudflare API."
      echo "    Do NOT use a wildcard (*) record — it is not needed and will cause conflicts."
    else
      echo "    You chose ${BOLD}master_proxy${NC} mode. Create this DNS record (DNS-only, grey cloud):"
      echo ""
      echo -e "      ${YELLOW}*${NC}  .${DOMAIN}  →  ${ip_display}"
      echo ""
      echo "    This wildcard record routes all subdomains (*.${DOMAIN}) to this server."
    fi
    echo ""
  fi

  echo "  Next steps:"
  echo "    1. Open the dashboard and create an admin account"
  echo "    2. Deploy apps using any of these methods:"
  echo "       a) GitHub Actions:  add a workflow that uses l8b-action"
  echo "       b) CLI:        curl -fsSL ${L8B_IN} | bash -s cli  then  l8b ship"
  echo "       c) Dashboard:  add from the web UI(only prebuilt images)"
  echo ""

  echo -e "  Manage LiteBin:  ${DIM}cd ${install_dir} && docker compose logs -f${NC}"

  # Save installed version
  echo "$release_url" > "${install_dir}/.version"
}

# -- Agent Server Setup ------------------------------------------------------
show_cert_bundle() {
  local install_dir certs_dir
  install_dir=$(find_install_dir "LiteBin installation not found. Run the master setup first.")
  certs_dir=$(find_certs_dir "$install_dir")

  [ -f "${certs_dir}/ca.pem" ] || die "No certificates found. Generate certs first."
  [ -f "${certs_dir}/agent.pem" ] || die "Agent certificate not found. Generate certs first."
  [ -f "${certs_dir}/agent-key.pem" ] || die "Agent key not found. Generate certs first."

  local cert_bundle
  cert_bundle=$(cat "${certs_dir}/ca.pem" "${certs_dir}/agent.pem" "${certs_dir}/agent-key.pem" | gzip -9 | base64_encode)

  echo ""
  echo -e "  ${GREEN}${BOLD}Agent cert bundle:${NC}"
  echo ""
  echo -e "    ${CYAN}${cert_bundle}${NC}"
  echo ""
  echo -e "  Run this on your agent server:"
  echo ""
  echo -e "    ${DIM}curl -fsSL ${L8B_IN} | bash -s agent --update-certs${NC}"
  echo ""
}

manage_certs() {
  local install_dir certs_dir
  install_dir=$(find_install_dir "LiteBin installation not found. Run the master setup first.")
  certs_dir=$(find_certs_dir "$install_dir")

  echo ""
  echo -e "  ${BOLD}Multi-server Certificate Management${NC}"
  echo ""

  if [ -f "${certs_dir}/ca.pem" ]; then
    echo "    1) Regenerate certificates (invalidates all existing agents)"
    echo "    2) Show agent cert bundle (for connecting new/existing agents)"
    echo ""
    local choice
    echo -ne "  ${CYAN}Choose [1-2]:${NC} "
    _tty_read choice
    case "$choice" in
      2) show_cert_bundle ;;
      *) regenerate_certs ;;
    esac
  else
    echo "    1) Generate certificates (first time setup)"
    echo ""
    local choice
    echo -ne "  ${CYAN}Choose [1]:${NC} "
    _tty_read choice
    regenerate_certs
  fi
}

regenerate_certs() {
  local install_dir certs_dir

  # Only supported on the master (Linux)
  local platform
  platform=$(detect_platform)
  [ "$platform" != "linux" ] && die "Worker setup requires running on the master server (Linux)."

  install_dir=$(find_install_dir "LiteBin installation not found. Run the master setup first.")
  certs_dir=$(find_certs_dir "$install_dir")

  # Check if certs already exist
  if [ -f "${certs_dir}/ca.pem" ]; then
    echo ""
    echo -e "  ${YELLOW}Warning: Existing mTLS certificates found.${NC}"
    echo -e "  ${YELLOW}All connected agents will lose access until their certs are updated.${NC}"
    echo ""
    if ! prompt_yes "Continue and regenerate certificates?"; then
      info "Cancelled."
      exit 0
    fi
  fi

  # Get domain from .env
  local domain
  domain=$(grep '^DOMAIN=' "${install_dir}/.env" | cut -d= -f2)
  [ -z "$domain" ] && die "Domain not found in .env"

  info "Generating mTLS certificates (ECDSA P-256)..."

  mkdir -p "$certs_dir"
  local certs_tmp
  certs_tmp=$(mktemp -d)

  # Generate CA (ECDSA P-256)
  openssl ecparam -genkey -name prime256v1 -noout -out "${certs_tmp}/ca-key.pem" 2>/dev/null \
    || die "Failed to generate CA key (is openssl installed?)"
  chmod 600 "${certs_tmp}/ca-key.pem" 2>/dev/null || true
  openssl req -new -x509 -days 3650 \
    -key "${certs_tmp}/ca-key.pem" \
    -out "${certs_tmp}/ca.pem" \
    -subj "/CN=LiteBin Root CA/O=LiteBin" 2>/dev/null \
    || die "Failed to generate CA certificate"

  # Generate master cert (ECDSA P-256)
  openssl ecparam -genkey -name prime256v1 -noout -out "${certs_tmp}/server-key.pem" 2>/dev/null \
    || die "Failed to generate server key"
  chmod 600 "${certs_tmp}/server-key.pem" 2>/dev/null || true
  openssl req -new \
    -key "${certs_tmp}/server-key.pem" \
    -out "${certs_tmp}/server.csr" \
    -subj "/CN=${domain}/O=LiteBin Master" 2>/dev/null \
    || die "Failed to generate server CSR"

  printf "subjectAltName=DNS:%s" "$domain" > "${certs_tmp}/san.ext"
  openssl x509 -req -days 3650 \
    -in "${certs_tmp}/server.csr" \
    -CA "${certs_tmp}/ca.pem" \
    -CAkey "${certs_tmp}/ca-key.pem" \
    -CAcreateserial \
    -out "${certs_tmp}/server.pem" \
    -extfile "${certs_tmp}/san.ext" 2>/dev/null \
    || die "Failed to sign server certificate"

  # Copy master certs
  cp "${certs_tmp}/ca.pem" "${certs_dir}/ca.pem"
  cp "${certs_tmp}/server.pem" "${certs_dir}/server.pem"
  cp "${certs_tmp}/server-key.pem" "${certs_dir}/server-key.pem"
  chmod 600 "${certs_dir}/server-key.pem" 2>/dev/null || true

  # Generate node/agent cert (ECDSA P-256)
  openssl ecparam -genkey -name prime256v1 -noout -out "${certs_tmp}/node-key.pem" 2>/dev/null \
    || die "Failed to generate node key"
  chmod 600 "${certs_tmp}/node-key.pem" 2>/dev/null || true
  openssl req -new \
    -key "${certs_tmp}/node-key.pem" \
    -out "${certs_tmp}/node.csr" \
    -subj "/CN=agent/O=LiteBin Agent" 2>/dev/null \
    || die "Failed to generate node CSR"
  printf "subjectAltName=DNS:agent" > "${certs_tmp}/node-san.ext"
  openssl x509 -req -days 3650 \
    -in "${certs_tmp}/node.csr" \
    -CA "${certs_tmp}/ca.pem" \
    -CAkey "${certs_tmp}/ca-key.pem" \
    -CAcreateserial \
    -extfile "${certs_tmp}/node-san.ext" \
    -out "${certs_tmp}/node.pem" 2>/dev/null \
    || die "Failed to sign node certificate"

  # Copy agent certs (so show_cert_bundle can re-read them later)
  cp "${certs_tmp}/node.pem" "${certs_dir}/agent.pem"
  cp "${certs_tmp}/node-key.pem" "${certs_dir}/agent-key.pem"
  chmod 600 "${certs_dir}/agent-key.pem" 2>/dev/null || true

  local cert_bundle
  cert_bundle=$(cat "${certs_tmp}/ca.pem" "${certs_tmp}/node.pem" "${certs_tmp}/node-key.pem" | gzip -9 | base64_encode)

  rm -rf "${certs_tmp}"
  info "Certificates generated."

  # Add mTLS config to .env
  if ! grep -q "MASTER_CA_CERT_PATH" "${install_dir}/.env" 2>/dev/null; then
    cat >> "${install_dir}/.env" <<EOF

# Multi-server mTLS
MASTER_CA_CERT_PATH=/certs/ca.pem
MASTER_CLIENT_CERT_PATH=/certs/server.pem
MASTER_CLIENT_KEY_PATH=/certs/server-key.pem
HEARTBEAT_INTERVAL_SECS=30
EOF
    info "Added mTLS config to .env"
  fi

  # Add certs mount to docker-compose.yml if missing
  if ! grep -q "/certs:ro" "${install_dir}/docker-compose.yml" 2>/dev/null; then
    sed -i '/\.\.\/projects:\/app\/projects/a\      - '"${certs_dir}"':/certs:ro' "${install_dir}/docker-compose.yml"
    info "Added certs mount to docker-compose.yml"
  fi

  # Restart orchestrator to pick up new certs
  if docker ps --format '{{.Names}}' 2>/dev/null | grep -q "litebin-orchestrator"; then
    info "Restarting orchestrator to load certificates..."
    (cd "$install_dir" && docker compose up -d --build 2>&1 | tail -3)
  fi

  echo ""
  echo -e "  ${GREEN}${BOLD}Agent certificates ready!${NC}"
  echo ""
  echo -e "  Run this on your agent server:"
  echo ""
  echo -e "    ${DIM}curl -fsSL ${L8B_IN} | bash -s agent${NC}"
  echo ""
  echo -e "  When prompted, paste this cert bundle:"
  echo ""
  echo -e "    ${CYAN}${cert_bundle}${NC}"
  echo ""
  echo -e "  Then go to Dashboard -> Agents -> Add Agent to connect."
  echo -e "  Manage:  ${DIM}cd ${install_dir} && docker compose logs -f${NC}"
}

# -- Agent Install -----------------------------------------------------------
install_agent() {
  local platform arch

  platform=$(detect_platform)
  [ "$platform" != "linux" ] && die "Agent setup requires Linux (agent servers run on Linux)"
  arch=$(detect_arch)

  # -- --update-certs mode --------------------------------------------
  if [ "${1:-}" = "--update-certs" ]; then
    local install_dir certs_dir
    install_dir=$(find_install_dir "LiteBin agent not found. Run 'curl -fsSL ${L8B_IN} | bash -s agent' first.")
    certs_dir=$(find_certs_dir "$install_dir")

    echo ""
    echo -e "  ${BOLD}Update agent certificates${NC}"
    echo ""
    local cert_bundle
    echo -ne "  ${CYAN}Paste the base64 cert bundle from the master:${NC} "
    _tty_read cert_bundle
    [ -z "$cert_bundle" ] && die "Cert bundle is required"

    info "Updating certificates..."
    echo "$cert_bundle" | base64_decode | gunzip | unpack_cert_bundle "$certs_dir"
    chmod 600 "${certs_dir}/agent-key.pem"

    # Verify
    [ -f "${certs_dir}/ca.pem" ] || die "ca.pem not found in cert bundle"
    [ -f "${certs_dir}/agent.pem" ] || die "agent.pem not found in cert bundle"
    [ -f "${certs_dir}/agent-key.pem" ] || die "agent-key.pem not found in cert bundle"

    # Restart agent container
    if docker ps --format '{{.Names}}' 2>/dev/null | grep -q "litebin-agent"; then
      local agent_port="5083"
      local prev_port
      prev_port=$(docker port litebin-agent 2>/dev/null | head -1 | cut -d: -f2 || true)
      [ -n "$prev_port" ] && agent_port="$prev_port"

      info "Restarting agent with new certificates..."
      docker stop litebin-agent 2>/dev/null || true
      docker rm litebin-agent 2>/dev/null || true

      (cd "$install_dir/agent" && docker build -t litebin-agent . >/dev/null 2>&1)
      run_agent_container "$install_dir" "$certs_dir" "$agent_port" >/dev/null
    else
      warn "No running agent container found. Run 'install.sh agent' to start one."
    fi

    info "Certificates updated successfully."
    exit 0
  fi

  # -- Full agent setup --------------------------------------------------

  # Root check
  local install_dir certs_dir
  if [ "$(id -u)" -eq 0 ]; then
    install_dir="/opt/litebin"
    certs_dir="/etc/litebin/certs"
  else
    warn "Not running as root. Installing to ${HOME}/litebin"
    warn "Configure your firewall manually (ports 80, 443, 5083)."
    install_dir="${HOME}/litebin"
    certs_dir="${install_dir}/certs"
  fi

  ensure_docker

  # Get release
  local release_url
  release_url=$(get_latest_release)
  [ -z "$release_url" ] || [ "$release_url" = "null" ] && die "Failed to detect latest release"

  # Create directories
  mkdir -p "${install_dir}/agent"
  mkdir -p "$certs_dir"

  # Create agent Caddyfile (admin on 0.0.0.0 so agent can reach it from another container)
  cat > "${install_dir}/agent/Caddyfile" <<'AGENT_CADDYFILE'
{
    admin 0.0.0.0:2019
}

:80 {
    respond 502
}
AGENT_CADDYFILE

  # Download agent binary
  download_and_verify \
    "https://github.com/${REPO}/releases/download/${release_url}/litebin-agent-${arch}-linux" \
    "${install_dir}/agent/litebin-agent" \
    "agent (${arch})"
  chmod +x "${install_dir}/agent/litebin-agent"

  # Generate agent Dockerfile
  cat > "${install_dir}/agent/Dockerfile" <<'AGENT_DOCKERFILE'
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY litebin-agent /usr/local/bin/litebin-agent
RUN chmod +x /usr/local/bin/litebin-agent
WORKDIR /etc/litebin
RUN mkdir -p /etc/litebin/data
EXPOSE 8443
CMD ["/usr/local/bin/litebin-agent"]
AGENT_DOCKERFILE

  # -- Interactive prompts -----------------------------------------------
  echo ""
  echo -e "${BOLD}LiteBin Agent Setup${NC}"
  echo ""

  prompt "Agent port (host-side)" AGENT_PORT "5083"

  # Certs
  echo ""
  echo -e "  Paste the base64 cert bundle from the master setup or"
  echo -e "  ${DIM}(Run 'curl -fsSL ${L8B_IN} | bash -s certs --show-bundle' on the master to get one.)${NC}"
  echo ""
  local cert_bundle
  echo -ne "${CYAN}Cert bundle:${NC} "
  _tty_read cert_bundle
  [ -z "$cert_bundle" ] && die "Cert bundle is required"

  info "Decoding certificates..."
  echo "$cert_bundle" | base64_decode | gunzip | unpack_cert_bundle "$certs_dir"
  chmod 600 "${certs_dir}/agent-key.pem"

  # Verify certs exist
  [ -f "${certs_dir}/ca.pem" ] || die "ca.pem not found in cert bundle"
  [ -f "${certs_dir}/agent.pem" ] || die "agent.pem not found in cert bundle"
  [ -f "${certs_dir}/agent-key.pem" ] || die "agent-key.pem not found in cert bundle"

  # -- Generate .env ---------------------------------------------------
  cat > "${install_dir}/agent/.env" <<EOF
# LiteBin Agent Configuration
# Generated by install.sh on $(date -u +%Y-%m-%dT%H:%M:%SZ)

# Server
AGENT_PORT=8443

# Certs (container paths)
AGENT_CERT_PATH=/certs/agent.pem
AGENT_KEY_PATH=/certs/agent-key.pem
AGENT_CA_CERT_PATH=/certs/ca.pem

# Docker
DOCKER_NETWORK=litebin-network

# Caddy sidecar
AGENT_CADDY_ADMIN_URL=http://litebin-agent-caddy:2019
AGENT_CADDY_CONTAINER_NAME=litebin-agent-caddy
EOF
  chmod 600 "${install_dir}/agent/.env" 2>/dev/null || true

  # -- Build and start --------------------------------------------------
  info "Building agent image..."
  (cd "${install_dir}/agent" && docker build -t litebin-agent .)

  # Stop existing containers if present
  docker stop litebin-agent-caddy 2>/dev/null || true
  docker rm litebin-agent-caddy 2>/dev/null || true
  docker stop litebin-agent 2>/dev/null || true
  docker rm litebin-agent 2>/dev/null || true

  # Start Caddy sidecar for agent local proxying
  info "Starting agent Caddy sidecar..."
  run_agent_caddy "$install_dir"

  info "Starting agent..."
  run_agent_container "$install_dir" "$certs_dir" "$AGENT_PORT"

  # -- Firewall ---------------------------------------------------------
  if [ "$(id -u)" -eq 0 ]; then
    configure_ufw
  fi

  # -- Done -------------------------------------------------------------
  # Save installed version
  echo "$release_url" > "${install_dir}/.version"

  echo ""
  echo -e "${GREEN}${BOLD}  Agent is running!${NC}"
  echo ""
  echo "  Agent port: ${AGENT_PORT}"
  echo ""
  echo "  Next steps:"
  echo "    1. Open the master dashboard"
  echo "    2. Go to Agents -> Add Agent"
  echo "    3. Enter this server's public IP and port ${AGENT_PORT}"
  echo "    4. Click 'Connect' to register the agent"
  echo ""
  echo -e "  View logs: ${DIM}docker logs -f litebin-agent${NC}"
}

# -- Master Update -----------------------------------------------------------
update_master() {
  local platform arch

  platform=$(detect_platform)
  arch=$(detect_arch)

  # Find install directory
  local install_dir
  install_dir=$(find_install_dir "LiteBin not found. Run 'curl -fsSL ${L8B_IN} | bash -s master' to install.")

  ensure_docker
  ensure_docker_compose

  # Get current version
  local current_version="unknown"
  if [ -f "${install_dir}/.version" ]; then
    current_version=$(cat "${install_dir}/.version")
  fi

  # Get latest release
  local latest_release
  latest_release=$(get_latest_release)

  echo ""
  echo -e "${BOLD}LiteBin Update${NC}"
  echo ""
  echo -e "  Current version:  ${CYAN}${current_version}${NC}"
  echo -e "  Latest version:   ${CYAN}${latest_release}${NC}"
  echo ""
  echo -e "  Changelog: ${DIM}${CHANGELOG_URL}${NC}"
  echo ""

  local target_release

  if [ "$current_version" = "$latest_release" ]; then
    if ! prompt_yes "Already up to date. Reinstall?"; then
      info "Cancelled."
      exit 0
    fi
    target_release="$latest_release"
  else
    echo "  Options:"
    echo -e "    1) Update to ${CYAN}${latest_release}${NC} (latest)"
    echo "    2) Enter a specific version"
    echo ""
    local choice
    echo -ne "  ${CYAN}Choose [1]:${NC} "
    _tty_read choice
    case "${choice:-1}" in
      2)
        prompt "Enter version (e.g. v0.1.2)" target_release ""
        [ -z "$target_release" ] && die "Version is required"
        # Verify release exists on GitHub
        if [ -z "${L8B_RELEASE_DIR:-}" ]; then
          if ! curl -sf "https://api.github.com/repos/${REPO}/releases/tags/${target_release}" 2>/dev/null | grep -q '"tag_name"'; then
            die "Release ${target_release} not found. Check available releases at ${CHANGELOG_URL}"
          fi
        fi
        ;;
      *)
        target_release="$latest_release"
        ;;
    esac
  fi

  # Confirm
  echo ""
  if [ "$current_version" = "$target_release" ]; then
    if ! prompt_yes "Reinstall ${current_version}?"; then
      info "Cancelled."
      exit 0
    fi
  elif ! prompt_yes "Update from ${current_version} to ${target_release}?"; then
    info "Cancelled."
    exit 0
  fi

  # Early check: docker-compose.yml changes (before downloading anything)
  # If compose changed, user can review and decide to proceed or exit
  local compose_changed=false
  local tmp_compose
  tmp_compose=$(mktemp)

  local certs_dir=""
  if grep -q "MASTER_CA_CERT_PATH" "${install_dir}/.env" 2>/dev/null; then
    certs_dir=$(find_certs_dir "$install_dir")
  fi
  generate_compose "$tmp_compose" "$certs_dir"

  if ! diff -q "$tmp_compose" "${install_dir}/docker-compose.yml" >/dev/null 2>&1; then
    compose_changed=true
    echo ""
    warn "docker-compose.yml needs to be updated in this version:"
    echo ""
    diff "${install_dir}/docker-compose.yml" "$tmp_compose" || true
    echo ""
    echo -e "  Review the changelog for details: ${DIM}${CHANGELOG_URL}${NC}"
    echo ""
    if ! prompt_yes "Apply these changes?"; then
      info "Skipped. Update docker-compose.yml manually if needed, then re-run the update."
      rm -f "$tmp_compose"
      exit 0
    fi
    cp "$tmp_compose" "${install_dir}/docker-compose.yml"
    info "docker-compose.yml updated"
  fi
  rm -f "$tmp_compose"

  # Backup database before restart
  info "Backing up database..."
  local backup_ok=false
  if docker ps --format '{{.Names}}' 2>/dev/null | grep -q "litebin-orchestrator"; then
    local backup_path="${install_dir}/backups/litebin.db.$(date +%Y%m%d%H%M%S)"
    mkdir -p "${install_dir}/backups"
    if docker cp litebin-orchestrator:/app/data/litebin.db "$backup_path" 2>/dev/null; then
      info "Database backed up to ${backup_path}"
      backup_ok=true
    fi
  fi
  if [ "$backup_ok" = false ]; then
    warn "Could not backup database (orchestrator not running or no data yet)"
  fi

  # Download new orchestrator binary
  download_and_verify \
    "https://github.com/${REPO}/releases/download/${target_release}/litebin-orchestrator-${arch}-linux" \
    "${install_dir}/orchestrator/litebin-orchestrator" \
    "orchestrator ${target_release} (${arch})"
  chmod +x "${install_dir}/orchestrator/litebin-orchestrator"

  # Download new dashboard
  if [ -n "${L8B_RELEASE_DIR:-}" ] && [ -d "${L8B_RELEASE_DIR}/l8b-dashboard-dist" ]; then
    info "Using local dashboard..."
    mkdir -p "${install_dir}/dashboard/dist"
    cp -r "${L8B_RELEASE_DIR}/l8b-dashboard-dist/." "${install_dir}/dashboard/dist/"
  else
    local tmp_tar="/tmp/l8b-dashboard.tar.gz"
    download_and_verify \
      "https://github.com/${REPO}/releases/download/${target_release}/l8b-dashboard.tar.gz" \
      "$tmp_tar" \
      "dashboard ${target_release}"
    tar -xzf "$tmp_tar" -C "${install_dir}/dashboard/"
    rm -f "$tmp_tar"
  fi

  # Save installed version
  echo "$target_release" > "${install_dir}/.version"

  # Confirm restart
  echo ""
  if ! prompt_yes "Restart LiteBin now?"; then
    info "Update ready. Restart manually when ready:"
    echo -e "  ${DIM}cd ${install_dir} && docker compose up -d --build${NC}"
    exit 0
  fi

  # Restart
  info "Restarting LiteBin..."
  (cd "$install_dir" && docker compose up -d --build 2>&1 | tail -5)

  # Verify
  echo ""
  info "Waiting for services to start..."
  sleep 3

  if [ "$(docker inspect -f '{{.State.Running}}' litebin-orchestrator 2>/dev/null)" = "true" ]; then
    echo ""
    echo -e "${GREEN}${BOLD}  LiteBin ${target_release} is running!${NC}"
    echo ""
    echo -e "  Manage:  ${DIM}cd ${install_dir} && docker compose logs -f${NC}"
  else
    warn "Orchestrator may not have started successfully."
    echo -e "  Check logs: ${DIM}cd ${install_dir} && docker compose logs -f orchestrator${NC}"
  fi
}

# -- Auto-detect Update ------------------------------------------------------
update_litebin() {
  local is_master=false is_agent=false
  local master_dir="" agent_dir=""

  # Detect master
  if [ "$(id -u)" -eq 0 ] && [ -f "/opt/litebin/docker-compose.yml" ]; then
    master_dir="/opt/litebin"
  elif [ -f "${HOME}/litebin/docker-compose.yml" ]; then
    master_dir="${HOME}/litebin"
  fi
  [ -n "$master_dir" ] && is_master=true

  # Detect agent
  if docker ps --format '{{.Names}}' 2>/dev/null | grep -q "^litebin-agent$"; then
    is_agent=true
  fi

  # Nothing found
  if [ "$is_master" = false ] && [ "$is_agent" = false ]; then
    die "No LiteBin installation found. Run 'curl -fsSL ${L8B_IN} | bash -s master' or 'bash -s agent' first."
  fi

  # Both found
  if [ "$is_master" = true ] && [ "$is_agent" = true ]; then
    echo ""
    echo -e "  ${YELLOW}Both master and agent detected on this server.${NC}"
    echo ""
    echo "    1) Update master"
    echo "    2) Update agent"
    echo ""
    local choice
    echo -ne "  ${CYAN}Choose [1-2]:${NC} "
    _tty_read choice
    case "$choice" in
      2) update_agent ;;
      *) update_master ;;
    esac
    return
  fi

  # Only one found
  if [ "$is_master" = true ]; then
    update_master
  else
    update_agent
  fi
}

# -- Agent Update -------------------------------------------------------------
update_agent() {
  local arch
  arch=$(detect_arch)

  local install_dir
  install_dir=$(find_install_dir)
  [ -d "${install_dir}/agent" ] || die "Agent installation not found at ${install_dir}/agent"

  # Get current version
  local current_version="unknown"
  if [ -f "${install_dir}/.version" ]; then
    current_version=$(cat "${install_dir}/.version")
  fi

  # Get latest release
  local latest_release
  latest_release=$(get_latest_release)

  echo ""
  echo -e "${BOLD}LiteBin Agent Update${NC}"
  echo ""
  echo -e "  Current version:  ${CYAN}${current_version}${NC}"
  echo -e "  Latest version:   ${CYAN}${latest_release}${NC}"
  echo ""

  if [ "$current_version" = "$latest_release" ]; then
    if ! prompt_yes "Already up to date. Reinstall?"; then
      info "Cancelled."
      exit 0
    fi
  else
    if ! prompt_yes "Update agent from ${current_version} to ${latest_release}?"; then
      info "Cancelled."
      exit 0
    fi
  fi

  ensure_docker

  # Detect port from previous container, fallback to 5083
  local agent_port="5083"
  local prev_port
  prev_port=$(docker port litebin-agent 2>/dev/null | head -1 | cut -d: -f2 || true)
  [ -n "$prev_port" ] && agent_port="$prev_port"

  # Download new agent binary
  download_and_verify \
    "https://github.com/${REPO}/releases/download/${latest_release}/litebin-agent-${arch}-linux" \
    "${install_dir}/agent/litebin-agent" \
    "agent ${latest_release} (${arch})"
  chmod +x "${install_dir}/agent/litebin-agent"

  # Rebuild and restart
  info "Rebuilding agent image..."
  (cd "${install_dir}/agent" && docker build -t litebin-agent .)

  local certs_dir
  certs_dir=$(find_certs_dir "$install_dir")

  # Restart Caddy sidecar FIRST so the agent can push config to it on startup
  info "Restarting agent Caddy sidecar..."
  run_agent_caddy "$install_dir"

  info "Restarting agent..."
  docker stop litebin-agent 2>/dev/null || true
  docker rm litebin-agent 2>/dev/null || true
  run_agent_container "$install_dir" "$certs_dir" "$agent_port"

  # Save installed version
  echo "$latest_release" > "${install_dir}/.version"

  echo ""
  echo -e "${GREEN}${BOLD}  Agent ${latest_release} is running!${NC}"
  echo ""
  echo -e "  View logs: ${DIM}docker logs -f litebin-agent${NC}"
}

# -- Interactive Menu --------------------------------------------------------
show_menu() {
  # If no terminal at all (piped in CI/script), print usage and exit
  if ! [ -t 0 ] && ! [ -t 1 ]; then
    local latest
    latest=$(get_latest_release 2>/dev/null) || true
    echo -e "${BOLD}LiteBin Installer${NC}  ${latest:-}"
    echo ""
    echo "Usage:  curl -fsSL ${L8B_IN} | bash -s <mode>"
    echo ""
    echo "Modes:"
    echo "  master    Set up the master server (orchestrator + dashboard + Caddy)"
    echo "  agent     Set up an agent server (Linux only)"
    echo "  cli       Install the l8b CLI tool"
    echo "  update    Update LiteBin (auto-detects master or agent)"
    echo ""
    echo "Examples:"
    echo "  curl -fsSL ${L8B_IN} | bash -s master"
    echo "  curl -fsSL ${L8B_IN} | bash -s agent"
    echo "  curl -fsSL ${L8B_IN} | bash -s cli"
    echo "  curl -fsSL ${L8B_IN} | bash -s update"
    exit 1
  fi

  echo ""
  echo -e "  ${PURPLE}██╗      █████╗ ██████╗ ██╗███╗   ██╗${NC}"
  echo -e "  ${PURPLE}██║     ██╔══██╗██╔══██╗██║████╗  ██║${NC}"
  echo -e "  ${PURPLE}██║     ╚█████╔╝██████╔╝██║██╔██╗ ██║${NC}"
  echo -e "  ${PURPLE}██║     ██╔══██╗██╔══██╗██║██║╚██╗██║${NC}"
  echo -e "  ${PURPLE}███████╗╚█████╔╝██████╔╝██║██║ ╚████║${NC}"
  echo -e "  ${PURPLE}╚══════╝ ╚════╝ ╚═════╝ ╚═╝╚═╝  ╚═══╝${NC}"
  echo ""
  local latest
  latest=$(get_latest_release 2>/dev/null) || true
  echo -e "  ${BOLD}LiteBin Installer${NC}  ${DIM}${latest:-}${NC}"
  echo ""
  echo "  What would you like to install?"
  echo ""
  echo "    1) Master Server (orchestrator + dashboard + Caddy)"
  echo "    2) Agent (worker daemon)"
  echo "    3) CLI only (l8b deploy tool)"
  echo "    4) Multi-server certs (generate / regenerate / show bundle)"
  echo "    5) Update LiteBin"
  echo ""
  local choice
  echo -ne "  ${CYAN}Enter your choice [1-5]:${NC} "
  _tty_read choice
  echo ""

  case "$choice" in
    1) install_master ;;
    2) install_agent ;;
    3) install_cli ;;
    4) manage_certs ;;
    5) update_litebin ;;
    *) die "Invalid choice. Enter 1, 2, 3, 4, or 5." ;;
  esac
}

# -- Certs CLI helper --------------------------------------------------------
certs_cmd() {
  case "${1:-}" in
    --show-bundle) show_cert_bundle ;;
    --regenerate)  regenerate_certs ;;
    "")            manage_certs ;;
    *)             die "Unknown certs option: $1. Use --show-bundle or --regenerate." ;;
  esac
}

# -- Main --------------------------------------------------------------------
case "${1:-}" in
  master)  install_master ;;
  agent)   install_agent "${2:-}" ;;
  cli)     install_cli ;;
  certs)   certs_cmd "${2:-}" ;;
  update)  update_litebin ;;
  -h|--help|"help")
    local latest
    latest=$(get_latest_release 2>/dev/null) || true
    echo -e "${BOLD}LiteBin Installer${NC}  ${latest:-}"
    echo ""
    echo "Usage:  curl -fsSL ${L8B_IN} | bash -s <mode>"
    echo ""
    echo "Modes:"
    echo "  master    Set up the master server (orchestrator + dashboard + Caddy)"
    echo "  agent     Set up an agent server (Linux only)"
    echo "  cli       Install the l8b CLI tool"
    echo "  update    Update LiteBin (auto-detects master or agent)"
    echo "  certs     Manage mTLS certs (interactive, or use --show-bundle / --regenerate)"
    ;;
  "")       show_menu ;;
  *)        die "Unknown mode: $1. Run with --help for usage." ;;
esac
