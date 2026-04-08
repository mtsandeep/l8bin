#!/usr/bin/env bash
set -euo pipefail

REPO="mtsandeep/l8bin"
L8B_IN="${L8B_IN:-https://l8b.in}"

# ── Colors ──────────────────────────────────────────────────────────────────
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

prompt() {
  local msg="$1" var="$2" default="${3:-}"
  if [ -n "$default" ]; then
    echo -ne "${CYAN}${msg}${NC} [${default}]: "
  else
    echo -ne "${CYAN}${msg}${NC}: "
  fi
  read -r value
  eval "$var=\"${value:-$default}\""
}

prompt_yes() {
  local msg="$1" default="${2:-n}"
  local yn
  echo -ne "${CYAN}${msg}${NC} [${default}]: "
  read -r yn
  yn="${yn:-$default}"
  [[ "$yn" =~ ^[Yy]$ ]]
}

# ── Platform detection ─────────────────────────────────────────────────────
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

# ── Helpers ─────────────────────────────────────────────────────────────────
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

configure_ufw() {
  if command -v ufw &>/dev/null; then
    info "Configuring UFW firewall..."
    ufw --force reset
    ufw default deny incoming
    ufw default allow outgoing
    ufw allow 80/tcp
    ufw allow 443/tcp
    ufw allow 443/udp
    ufw allow 5083/tcp
    ufw --force enable
    info "UFW configured."
  else
    warn "UFW not found. Configure your firewall manually (ports 80, 443, 5083)."
  fi
}

# ── CLI Install ─────────────────────────────────────────────────────────────
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

# ── Master Install ──────────────────────────────────────────────────────────
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
EXPOSE 5080
CMD ["/app/litebin-orchestrator"]
ORCH_DOCKERFILE

  # Generate dashboard Dockerfile
  cat > "${install_dir}/dashboard/Dockerfile" <<'DASH_DOCKERFILE'
FROM nginx:alpine
COPY dist/ /usr/share/nginx/html/
EXPOSE 80
DASH_DOCKERFILE

  # ── Interactive prompts ────────────────────────────────────────────────
  echo ""
  echo -e "${BOLD}LiteBin Master Setup${NC}"
  echo ""

  local domain_default="localhost"
  if is_linux; then
    domain_default=""
  fi

  prompt "Domain name (e.g. example.com)" DOMAIN "$domain_default"
  [ -z "$DOMAIN" ] && die "Domain is required"

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
    read -r mode_choice
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

  # Multi-node certs (skip on localhost/Windows dev)
  local multi_node="false"
  local cert_bundle=""
  if [ "$DOMAIN" != "localhost" ]; then
    echo ""
    if prompt_yes "Will you connect worker nodes?"; then
      multi_node="true"
      info "Generating mTLS certificates..."

      local certs_tmp
      certs_tmp=$(mktemp -d)

      # Generate CA
      openssl genrsa -out "${certs_tmp}/ca-key.pem" 4096 2>/dev/null
      chmod 600 "${certs_tmp}/ca-key.pem" 2>/dev/null || true
      openssl req -new -x509 -days 3650 \
        -key "${certs_tmp}/ca-key.pem" \
        -out "${certs_tmp}/ca.pem" \
        -subj "/CN=LiteBin Root CA/O=LiteBin" 2>/dev/null

      # Generate master cert
      openssl genrsa -out "${certs_tmp}/server-key.pem" 4096 2>/dev/null
      chmod 600 "${certs_tmp}/server-key.pem" 2>/dev/null || true
      openssl req -new \
        -key "${certs_tmp}/server-key.pem" \
        -out "${certs_tmp}/server.csr" \
        -subj "/CN=${DOMAIN}/O=LiteBin Master" 2>/dev/null
      openssl x509 -req -days 3650 \
        -in "${certs_tmp}/server.csr" \
        -CA "${certs_tmp}/ca.pem" \
        -CAkey "${certs_tmp}/ca-key.pem" \
        -CAcreateserial \
        -out "${certs_tmp}/server.pem" \
        -extfile <(printf "subjectAltName=DNS:%s" "$DOMAIN") 2>/dev/null

      # Copy master certs to certs dir
      cp "${certs_tmp}/ca.pem" "${certs_dir}/ca.pem"
      cp "${certs_tmp}/server.pem" "${certs_dir}/server.pem"
      cp "${certs_tmp}/server-key.pem" "${certs_dir}/server-key.pem"
      chmod 600 "${certs_dir}/server-key.pem" 2>/dev/null || true

      # Generate a node cert for the first agent
      local node_name
      prompt "Name for the first worker node" node_name "worker-1"

      openssl genrsa -out "${certs_tmp}/node-key.pem" 4096 2>/dev/null
      chmod 600 "${certs_tmp}/node-key.pem" 2>/dev/null || true
      openssl req -new \
        -key "${certs_tmp}/node-key.pem" \
        -out "${certs_tmp}/node.csr" \
        -subj "/CN=${node_name}/O=LiteBin Node" 2>/dev/null
      openssl x509 -req -days 3650 \
        -in "${certs_tmp}/node.csr" \
        -CA "${certs_tmp}/ca.pem" \
        -CAkey "${certs_tmp}/ca-key.pem" \
        -CAcreateserial \
        -out "${certs_tmp}/node.pem" 2>/dev/null

      # Base64 encode the cert bundle for the agent
      cert_bundle=$(tar -cf - -C "${certs_tmp}" ca.pem node.pem node-key.pem | base64_encode)

      rm -rf "${certs_tmp}"
      info "Certificates generated."
    fi
  fi

  # ── Generate .env ─────────────────────────────────────────────────────
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
DOCKER_NETWORK=litebin-apps

# Server
HOST=0.0.0.0
PORT=5080

# Sleep / Janitor
IDLE_TIMEOUT_SECS=900
JANITOR_INTERVAL_SECS=300

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

  if [ "$multi_node" = "true" ]; then
    cat >> "${install_dir}/.env" <<EOF

# Multi-node mTLS
MASTER_CA_CERT_PATH=/certs/ca.pem
MASTER_CLIENT_CERT_PATH=/certs/server.pem
MASTER_CLIENT_KEY_PATH=/certs/server-key.pem
HEARTBEAT_INTERVAL_SECS=30
EOF
  fi

  chmod 600 "${install_dir}/.env" 2>/dev/null || true

  # ── Generate Caddyfile ────────────────────────────────────────────────
  cat > "${install_dir}/Caddyfile" <<'CADDYFILE'
{
	admin 0.0.0.0:2019
}

:80 {
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
	handle /deploy-tokens* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /images/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /nodes* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /settings* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /health {
		reverse_proxy litebin-orchestrator:5080
	}
	handle /caddy/* {
		reverse_proxy litebin-orchestrator:5080
	}
	handle {
		reverse_proxy litebin-dashboard:80
	}
}
CADDYFILE

  # ── Generate docker-compose.yml ──────────────────────────────────────
  local certs_mount=""
  if [ "$multi_node" = "true" ]; then
    certs_mount="      - ${certs_dir}:/certs:ro"
  fi

  cat > "${install_dir}/docker-compose.yml" <<COMPOSE_EOF
services:
  orchestrator:
    build: ./orchestrator
    container_name: litebin-orchestrator
    restart: unless-stopped
    ports:
      - "5080:5080"
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - orchestrator-data:/app/data
      - ./projects:/app/projects
${certs_mount}
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
    image: caddy:2-alpine
    container_name: litebin-caddy
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
      - "443:443/udp"
    extra_hosts:
      - "host.docker.internal:host-gateway"
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy-data:/data
      - caddy-config:/config
      - caddy-root:/root/.local/share/caddy
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

  # ── Configure firewall (Linux only) ──────────────────────────────────
  if is_linux && [ "$(id -u)" -eq 0 ]; then
    configure_ufw
  fi

  # ── Start ────────────────────────────────────────────────────────────
  echo ""
  info "Starting LiteBin..."
  (cd "$install_dir" && docker compose up -d --build)

  # ── Done ─────────────────────────────────────────────────────────────
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
  echo -e "  API:        ${CYAN}http://${DOMAIN}:5080${NC}"
  echo ""
  echo "  Next steps:"
  echo "    1. Open the dashboard and create an admin account"
  echo "    2. Deploy apps using any of these methods:"
  echo "       a) GitHub Actions:  add a workflow that uses l8b-action"
  echo "       b) CLI:        curl -sSL ${L8B_IN} | sh -s cli  then  l8b ship"
  echo "       c) Dashboard:  add from the web UI(only prebuilt images)"
  echo ""

  if [ "$multi_node" = "true" ]; then
    echo -e "  ${BOLD}Worker node setup:${NC}"
    echo "    Run this on your worker node:"
    echo ""
    echo -e "    ${DIM}curl -sSL ${L8B_IN} | sh -s agent${NC}"
    echo ""
    echo -e "    When prompted, paste this cert bundle:"
    echo ""
    echo -e "    ${CYAN}${cert_bundle}${NC}"
    echo ""
    echo -e "    ${DIM}(This bundle is shown only once. Save it if needed for additional nodes.)${NC}"
    echo ""
  fi

  echo -e "  Manage LiteBin:  ${DIM}cd ${install_dir} && docker compose logs -f${NC}"
}

# ── Cert Regeneration ──────────────────────────────────────────────────────
regenerate_certs() {
  local install_dir certs_dir

  # Cert regeneration is only supported on the master (Linux)
  local platform
  platform=$(detect_platform)
  [ "$platform" != "linux" ] && die "Certificate regeneration requires running on the master server (Linux)."

  # Find existing install directory
  if [ "$(id -u)" -eq 0 ] && [ -d "/opt/litebin" ]; then
    install_dir="/opt/litebin"
  elif [ -d "${HOME}/litebin" ]; then
    install_dir="${HOME}/litebin"
  else
    die "LiteBin installation not found. Run 'install.sh master' first."
  fi

  certs_dir="${install_dir}/certs"

  # Check if certs already exist
  if [ -f "${certs_dir}/ca.pem" ]; then
    echo ""
    echo -e "  ${YELLOW}Warning: Existing mTLS certificates found in ${certs_dir}${NC}"
    echo -e "  ${YELLOW}All connected worker nodes will lose access until their certs are updated.${NC}"
    echo ""
    if ! prompt_yes "Continue and regenerate certificates?"; then
      info "Cancelled."
      exit 0
    fi
  fi

  # Prompt for domain (needed for master cert SAN)
  local domain
  if [ -f "${install_dir}/.env" ]; then
    domain=$(grep '^DOMAIN=' "${install_dir}/.env" | cut -d= -f2)
  fi
  if [ -z "$domain" ]; then
    domain=$(prompt "Domain name (used for master certificate)" DOMAIN "")
  fi
  [ -z "$domain" ] && die "Domain is required"

  info "Regenerating mTLS certificates..."

  mkdir -p "$certs_dir"
  local certs_tmp
  certs_tmp=$(mktemp -d)

  # Generate CA
  openssl genrsa -out "${certs_tmp}/ca-key.pem" 4096 2>/dev/null \
    || die "Failed to generate CA key (is openssl installed?)"
  chmod 600 "${certs_tmp}/ca-key.pem" 2>/dev/null || true
  openssl req -new -x509 -days 3650 \
    -key "${certs_tmp}/ca-key.pem" \
    -out "${certs_tmp}/ca.pem" \
    -subj "/CN=LiteBin Root CA/O=LiteBin" 2>/dev/null \
    || die "Failed to generate CA certificate"

  # Generate master cert
  openssl genrsa -out "${certs_tmp}/server-key.pem" 4096 2>/dev/null \
    || die "Failed to generate server key"
  chmod 600 "${certs_tmp}/server-key.pem" 2>/dev/null || true
  openssl req -new \
    -key "${certs_tmp}/server-key.pem" \
    -out "${certs_tmp}/server.csr" \
    -subj "/CN=${domain}/O=LiteBin Master" 2>/dev/null \
    || die "Failed to generate server CSR"

  # Write SAN extension to a file (process substitution doesn't work on all shells)
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

  # Generate a node cert for the first agent
  local node_name
  prompt "Name for the first worker node (or leave blank to skip)" node_name "worker-1"

  local cert_bundle=""
  if [ -n "$node_name" ]; then
    openssl genrsa -out "${certs_tmp}/node-key.pem" 4096 2>/dev/null \
      || die "Failed to generate node key"
    chmod 600 "${certs_tmp}/node-key.pem" 2>/dev/null || true
    openssl req -new \
      -key "${certs_tmp}/node-key.pem" \
      -out "${certs_tmp}/node.csr" \
      -subj "/CN=${node_name}/O=LiteBin Node" 2>/dev/null \
      || die "Failed to generate node CSR"
    openssl x509 -req -days 3650 \
      -in "${certs_tmp}/node.csr" \
      -CA "${certs_tmp}/ca.pem" \
      -CAkey "${certs_tmp}/ca-key.pem" \
      -CAcreateserial \
      -out "${certs_tmp}/node.pem" 2>/dev/null \
      || die "Failed to sign node certificate"

    cert_bundle=$(tar -cf - -C "${certs_tmp}" ca.pem node.pem node-key.pem | base64_encode)
  fi

  rm -rf "${certs_tmp}"
  info "Certificates regenerated."

  # Ensure .env has mTLS config
  if ! grep -q "MASTER_CA_CERT_PATH" "${install_dir}/.env" 2>/dev/null; then
    cat >> "${install_dir}/.env" <<EOF

# Multi-node mTLS
MASTER_CA_CERT_PATH=/certs/ca.pem
MASTER_CLIENT_CERT_PATH=/certs/server.pem
MASTER_CLIENT_KEY_PATH=/certs/server-key.pem
HEARTBEAT_INTERVAL_SECS=30
EOF
    info "Added mTLS config to .env"
  fi

  # Restart orchestrator to pick up new certs
  if docker ps --format '{{.Names}}' 2>/dev/null | grep -q "litebin-orchestrator"; then
    info "Restarting orchestrator to load new certificates..."
    (cd "$install_dir" && docker compose restart orchestrator 2>/dev/null)
  fi

  echo ""
  echo -e "  ${BOLD}Certificates regenerated!${NC}"
  echo ""

  if [ -n "$cert_bundle" ]; then
    echo -e "  ${BOLD}Update each worker node by running:${NC}"
    echo ""
    echo -e "    ${DIM}curl -sSL ${L8B_IN} | sh -s agent --update-certs${NC}"
    echo ""
    echo -e "    ${DIM}When prompted, paste this cert bundle:${NC}"
    echo ""
    echo -e "    ${CYAN}${cert_bundle}${NC}"
    echo ""
    echo -e "    ${DIM}(This bundle is shown only once. Save it if needed for additional nodes.)${NC}"
    echo ""
  else
    echo -e "  ${BOLD}To add a worker node, re-run:${NC}"
    echo -e "    ${DIM}curl -sSL ${L8B_IN} | sh -s certs${NC}"
    echo ""
  fi

  echo -e "  Manage LiteBin:  ${DIM}cd ${install_dir} && docker compose logs -f${NC}"
}

# ── Agent Install ───────────────────────────────────────────────────────────
install_agent() {
  local platform arch

  platform=$(detect_platform)
  [ "$platform" != "linux" ] && die "Agent setup requires Linux (worker nodes run on Linux servers)"
  arch=$(detect_arch)

  # ── --update-certs mode ────────────────────────────────────────────
  if [ "${1:-}" = "--update-certs" ]; then
    # Find certs dir
    local certs_dir
    if [ "$(id -u)" -eq 0 ] && [ -d "/etc/litebin/certs" ]; then
      certs_dir="/etc/litebin/certs"
    elif [ -d "${HOME}/litebin/certs" ]; then
      certs_dir="${HOME}/litebin/certs"
    else
      die "LiteBin agent not found. Run 'curl -sSL ${L8B_IN} | sh -s agent' first."
    fi

    echo ""
    echo -e "  ${BOLD}Update agent certificates${NC}"
    echo ""
    local cert_bundle
    echo -ne "  ${CYAN}Paste the base64 cert bundle from the master:${NC} "
    read -r cert_bundle
    [ -z "$cert_bundle" ] && die "Cert bundle is required"

    info "Updating certificates..."
    echo "$cert_bundle" | base64_decode | tar -xf - -C "$certs_dir"

    # Rename node certs to agent names
    if [ -f "${certs_dir}/node.pem" ]; then
      mv "${certs_dir}/node.pem" "${certs_dir}/agent.pem"
    fi
    if [ -f "${certs_dir}/node-key.pem" ]; then
      mv "${certs_dir}/node-key.pem" "${certs_dir}/agent-key.pem"
      chmod 600 "${certs_dir}/agent-key.pem"
    fi

    # Verify
    [ -f "${certs_dir}/ca.pem" ] || die "ca.pem not found in cert bundle"
    [ -f "${certs_dir}/agent.pem" ] || die "agent.pem not found in cert bundle"
    [ -f "${certs_dir}/agent-key.pem" ] || die "agent-key.pem not found in cert bundle"

    # Restart agent container
    if docker ps --format '{{.Names}}' 2>/dev/null | grep -q "litebin-agent"; then
      info "Restarting agent with new certificates..."
      docker stop litebin-agent 2>/dev/null || true
      docker rm litebin-agent 2>/dev/null || true

      local install_dir
      if [ "$(id -u)" -eq 0 ] && [ -d "/opt/litebin" ]; then
        install_dir="/opt/litebin"
      else
        install_dir="${HOME}/litebin"
      fi

      (cd "$install_dir/agent" && docker build -t litebin-agent . >/dev/null 2>&1)
      docker run -d \
        --name litebin-agent \
        --restart unless-stopped \
        -v /var/run/docker.sock:/var/run/docker.sock \
        -v "$certs_dir":/certs:ro \
        -v litebin-agent-data:/etc/litebin/data \
        -p 5083:8443 \
        -e AGENT_PORT=8443 \
        -e AGENT_CERT_PATH=/certs/agent.pem \
        -e AGENT_KEY_PATH=/certs/agent-key.pem \
        -e AGENT_CA_CERT_PATH=/certs/ca.pem \
        litebin-agent >/dev/null
    else
      warn "No running agent container found. Run 'install.sh agent' to start one."
    fi

    info "Certificates updated successfully."
    exit 0
  fi

  # ── Full agent setup ──────────────────────────────────────────────────

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
EXPOSE 8443
CMD ["/usr/local/bin/litebin-agent"]
AGENT_DOCKERFILE

  # ── Interactive prompts ───────────────────────────────────────────────
  echo ""
  echo -e "${BOLD}LiteBin Agent Setup${NC}"
  echo ""

  prompt "Master dashboard URL (e.g. https://l8bin.example.com)" MASTER_URL ""
  [ -z "$MASTER_URL" ] && die "Master URL is required"

  prompt "Node name" NODE_NAME "worker-1"
  prompt "Agent port (host-side)" AGENT_PORT "5083"

  # Certs
  echo ""
  echo -e "  Paste the base64 cert bundle from the master setup."
  echo -e "  ${DIM}(Run 'curl -sSL ${L8B_IN} | sh -s master' on the master to generate one.)${NC}"
  echo ""
  local cert_bundle
  echo -ne "${CYAN}Cert bundle:${NC} "
  read -r cert_bundle
  [ -z "$cert_bundle" ] && die "Cert bundle is required"

  info "Decoding certificates..."
  echo "$cert_bundle" | base64_decode | tar -xf - -C "$certs_dir"
  # Rename node certs to expected names
  if [ -f "${certs_dir}/node.pem" ]; then
    mv "${certs_dir}/node.pem" "${certs_dir}/agent.pem"
  fi
  if [ -f "${certs_dir}/node-key.pem" ]; then
    mv "${certs_dir}/node-key.pem" "${certs_dir}/agent-key.pem"
    chmod 600 "${certs_dir}/agent-key.pem"
  fi

  # Verify certs exist
  [ -f "${certs_dir}/ca.pem" ] || die "ca.pem not found in cert bundle"
  [ -f "${certs_dir}/agent.pem" ] || die "agent.pem not found in cert bundle"
  [ -f "${certs_dir}/agent-key.pem" ] || die "agent-key.pem not found in cert bundle"

  # ── Build and start ──────────────────────────────────────────────────
  info "Building agent image..."
  (cd "${install_dir}/agent" && docker build -t litebin-agent .)

  # Stop existing container if present
  docker stop litebin-agent 2>/dev/null || true
  docker rm litebin-agent 2>/dev/null || true

  info "Starting agent..."
  docker run -d \
    --name litebin-agent \
    --restart unless-stopped \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v "${certs_dir}":/certs:ro \
    -v litebin-agent-data:/etc/litebin/data \
    -p "${AGENT_PORT}:8443" \
    -e AGENT_PORT=8443 \
    -e AGENT_CERT_PATH=/certs/agent.pem \
    -e AGENT_KEY_PATH=/certs/agent-key.pem \
    -e AGENT_CA_CERT_PATH=/certs/ca.pem \
    litebin-agent

  # ── Firewall ─────────────────────────────────────────────────────────
  if [ "$(id -u)" -eq 0 ]; then
    configure_ufw
  fi

  # ── Done ─────────────────────────────────────────────────────────────
  echo ""
  echo -e "${GREEN}${BOLD}  Agent is running!${NC}"
  echo ""
  echo "  Node name:  ${NODE_NAME}"
  echo "  Agent port: ${AGENT_PORT}"
  echo ""
  echo "  Next steps:"
  echo "    1. Open the master dashboard: ${MASTER_URL}"
  echo "    2. Go to Nodes -> Add Node"
  echo "    3. Enter name '${NODE_NAME}' and this server's public IP"
  echo "    4. Click 'Connect' to register the agent"
  echo ""
  echo -e "  View logs: ${DIM}docker logs -f litebin-agent${NC}"
}

# ── Interactive Menu ────────────────────────────────────────────────────────
show_menu() {
  # If stdin is not a terminal, print help and exit
  if [ ! -t 0 ]; then
    echo -e "${BOLD}LiteBin Installer${NC}"
    echo ""
    echo "Usage:  curl -sSL ${L8B_IN} | sh -s <mode>"
    echo ""
    echo "Modes:"
    echo "  master    Set up the master server (orchestrator + dashboard + Caddy)"
    echo "  agent     Set up a worker node (Linux only)"
    echo "  cli       Install the l8b CLI tool"
    echo ""
    echo "Examples:"
    echo "  curl -sSL ${L8B_IN} | sh -s master"
    echo "  curl -sSL ${L8B_IN} | sh -s agent"
    echo "  curl -sSL ${L8B_IN} | sh -s cli"
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
  echo -e "  ${BOLD}LiteBin Installer${NC}"
  echo ""
  echo "  What would you like to install?"
  echo ""
  echo "    1) Master Server (orchestrator + dashboard + Caddy)"
  echo "    2) Agent Node (worker daemon)"
  echo "    3) CLI only (l8b deploy tool)"
  echo "    4) Regenerate mTLS certificates"
  echo ""
  local choice
  echo -ne "  ${CYAN}Enter your choice [1-4]:${NC} "
  read -r choice
  echo ""

  case "$choice" in
    1) install_master ;;
    2) install_agent ;;
    3) install_cli ;;
    4) regenerate_certs ;;
    *) die "Invalid choice. Enter 1, 2, 3, or 4." ;;
  esac
}

# ── Main ────────────────────────────────────────────────────────────────────
case "${1:-}" in
  master)  install_master ;;
  agent)   install_agent ;;
  cli)     install_cli ;;
  certs)   regenerate_certs ;;
  -h|--help|"help")
    echo -e "${BOLD}LiteBin Installer${NC}"
    echo ""
    echo "Usage:  curl -sSL ${L8B_IN} | sh -s <mode>"
    echo ""
    echo "Modes:"
    echo "  master    Set up the master server (orchestrator + dashboard + Caddy)"
    echo "  agent     Set up a worker node (Linux only)"
    echo "  cli       Install the l8b CLI tool"
    echo "  certs     Regenerate mTLS certificates (run on master)"
    ;;
  "")       show_menu ;;
  *)        die "Unknown mode: $1. Run with --help for usage." ;;
esac
