# install-windows.ps1
# Native PowerShell installer for LiteBin master on Windows.
# Run: iex (irm https://l8b.in/windows.ps1)
#
# Flags:
#   -Domain <domain>         Override domain (default: localhost)
#   -DashboardSub <sub>      Dashboard subdomain (default: l8bin)
#   -InstallDir <path>       Install directory (default: ~/litebin)
#   -Clean                   Remove installed containers and files
param(
    [string]$Domain = "localhost",
    [string]$DashboardSub = "l8bin",
    [string]$InstallDir = "",
    [switch]$Clean
)

$ErrorActionPreference = "Stop"
$Repo = "mtsandeep/l8bin"
$L8B_IN = "https://l8b.in"

# -- Colors ------------------------------------------------------------------
function Write-Step($msg) {
    Write-Host ""
    Write-Host "-- $msg --" -ForegroundColor Cyan
}

function Write-Info($msg) {
    Write-Host "==> $msg" -ForegroundColor Green
}

function Write-Err($msg) {
    Write-Host "Error: $msg" -ForegroundColor Red
}

# -- Clean ------------------------------------------------------------------
if ($Clean) {
    Write-Step "Cleaning up"
    $dir = if ($InstallDir) { $InstallDir } else { Join-Path $env:USERPROFILE "litebin" }
    $composeFile = Join-Path $dir "docker-compose.yml"
    if (Test-Path $composeFile) {
        Push-Location $dir
        $ErrorActionPreference = "SilentlyContinue"
        docker compose down -v --remove-orphans 2>&1 | Out-Null
        $ErrorActionPreference = "Stop"
        Pop-Location
    }
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
    Write-Info "Done."
    exit 0
}

# -- Banner -----------------------------------------------------------------
Write-Host ""
Write-Host "  ██╗      █████╗ ██████╗ ██╗███╗   ██╗" -ForegroundColor Magenta
Write-Host "  ██║     ██╔══██╗██╔══██╗██║████╗  ██║" -ForegroundColor Magenta
Write-Host "  ██║     ╚█████╔╝██████╔╝██║██╔██╗ ██║" -ForegroundColor Magenta
Write-Host "  ██║     ██╔══██╗██╔══██╗██║██║╚██╗██║" -ForegroundColor Magenta
Write-Host "  ███████╗╚█████╔╝██████╔╝██║██║ ╚████║" -ForegroundColor Magenta
Write-Host "  ╚══════╝ ╚════╝ ╚═════╝ ╚═╝╚═╝  ╚═══╝" -ForegroundColor Magenta
Write-Host ""
Write-Host "  LiteBin for Windows" -ForegroundColor White
Write-Host ""

# -- Prerequisites ----------------------------------------------------------
if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    Write-Err "Docker Desktop is required. Install it from https://docs.docker.com/desktop/setup/install/windows-install/"
    exit 1
}
Write-Info "Docker found: $(docker --version)"

docker compose version 2>$null | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Err "Docker Compose is not available."
    exit 1
}
Write-Info "Docker Compose available"

# -- Install directory -------------------------------------------------------
if (-not $InstallDir) {
    $InstallDir = Join-Path (Get-Location) "litebin"
}

# -- Check for existing installation -----------------------------------------
if ((Test-Path "$InstallDir\docker-compose.yml") -and (-not $Clean)) {
    $running = docker compose -f "$InstallDir\docker-compose.yml" ps --format "{{.Names}}" 2>$null
    if ($running) {
        Write-Host ""
        Write-Host "  Warning: LiteBin is already running in $InstallDir" -ForegroundColor Yellow
        $reply = Read-Host "  Continue and reinstall? [y/N]"
        if ($reply -notmatch '^[Yy]') {
            Write-Info "Cancelled."
            exit 0
        }
    }
}

$OrchDir = Join-Path $InstallDir "orchestrator"
$DashDir = Join-Path $InstallDir "dashboard"
$ProjectsDir = Join-Path $InstallDir "projects"

# -- Detect arch -------------------------------------------------------------
$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }

# -- Get latest release ------------------------------------------------------
$ReleaseDir = $env:L8B_RELEASE_DIR

if ($ReleaseDir) {
    $Tag = "local"
    $ReleaseBase = $ReleaseDir
} else {
    Write-Step "Fetching latest release"
    try {
        $response = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{"User-Agent"="PowerShell"}
        $Tag = $response.tag_name
        if (-not $Tag) { throw "No tag found" }
    } catch {
        Write-Err "Could not fetch latest release from GitHub. Check that https://github.com/$Repo/releases has at least one release."
        exit 1
    }
    Write-Info "Latest release: $Tag"
    $ReleaseBase = "https://github.com/$Repo/releases/download/$Tag"
}

# -- Create directories ------------------------------------------------------
foreach ($d in @($InstallDir, $OrchDir, $DashDir, $ProjectsDir)) {
    New-Item -ItemType Directory -Force -Path $d | Out-Null
}

# -- Orchestrator binary -----------------------------------------------------
$orchFile = Join-Path $OrchDir "litebin-orchestrator"

if ($ReleaseDir) {
    Write-Step "Using local orchestrator ($Arch)"
    $orchSrc = Join-Path $ReleaseDir "litebin-orchestrator-$Arch-linux"
    if (-not (Test-Path $orchSrc)) { Write-Err "Local file not found: $orchSrc"; exit 1 }
    Copy-Item $orchSrc $orchFile
} else {
    Write-Step "Downloading orchestrator ($Arch)"
    Invoke-WebRequest -Uri "$ReleaseBase/litebin-orchestrator-$Arch-linux" -OutFile $orchFile
}
$orchMB = [math]::Round((Get-Item $orchFile).Length / 1MB, 1)
Write-Info "  litebin-orchestrator-$Arch-linux  (${orchMB} MB)"

# -- Dashboard --------------------------------------------------------------
$dashDist = Join-Path $DashDir "dist"
New-Item -ItemType Directory -Force -Path $dashDist | Out-Null

if ($ReleaseDir -and (Test-Path (Join-Path $ReleaseDir "l8b-dashboard-dist"))) {
    Write-Step "Using local dashboard"
    Remove-Item $dashDist -Recurse -Force -ErrorAction SilentlyContinue
    Copy-Item (Join-Path $ReleaseDir "l8b-dashboard-dist") $dashDist -Recurse
} else {
    Write-Step "Downloading dashboard"
    $dashZip = Join-Path $InstallDir "l8b-dashboard.zip"
    Invoke-WebRequest -Uri "$ReleaseBase/l8b-dashboard.zip" -OutFile $dashZip
    Remove-Item $dashDist -Recurse -Force -ErrorAction SilentlyContinue
    Expand-Archive -Path $dashZip -DestinationPath $DashDir -Force
    Remove-Item $dashZip -ErrorAction SilentlyContinue
}
$dashKB = [math]::Round((Get-ChildItem $dashDist -Recurse | Measure-Object -Property Length -Sum).Sum / 1KB, 1)
Write-Info "  l8b-dashboard-dist  (${dashKB} KB)"

# -- Generate orchestrator Dockerfile ---------------------------------------
Set-Content -Path (Join-Path $OrchDir "Dockerfile") -Value @"
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY litebin-orchestrator /app/litebin-orchestrator
RUN chmod +x /app/litebin-orchestrator
WORKDIR /app
RUN mkdir -p /app/data
CMD ["/app/litebin-orchestrator"]
"@

# -- Generate dashboard Dockerfile ------------------------------------------
Set-Content -Path (Join-Path $DashDir "Dockerfile") -Value @"
FROM nginx:alpine
COPY dist/ /usr/share/nginx/html/
EXPOSE 80
"@

# -- Generate .env ----------------------------------------------------------
$Timestamp = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
Set-Content -Path (Join-Path $InstallDir ".env") -Value @"
# LiteBin Master Configuration
# Generated by install-windows.ps1 on $Timestamp

# Domain
DOMAIN=$Domain
DASHBOARD_SUBDOMAIN=$DashboardSub
POKE_SUBDOMAIN=poke

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

# Routing
ROUTING_MODE=master_proxy
"@

# -- Generate Caddyfile ----------------------------------------------------
Set-Content -Path (Join-Path $InstallDir "Caddyfile") -Value @"
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
"@

# -- Generate docker-compose.yml --------------------------------------------
Set-Content -Path (Join-Path $InstallDir "docker-compose.yml") -Value @"
services:
  orchestrator:
    build: ./orchestrator
    container_name: litebin-orchestrator
    restart: unless-stopped
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - orchestrator-data:/app/data
      - ./projects:/app/projects
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
    env_file:
      - .env
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
"@

# -- Start ------------------------------------------------------------------
Write-Step "Starting LiteBin"
Push-Location $InstallDir
docker compose up -d --build
Pop-Location

# -- Done -------------------------------------------------------------------
if ($Domain -eq "localhost") {
    $DashboardUrl = "https://${DashboardSub}.localhost"
} else {
    $DashboardUrl = "https://${DashboardSub}.${Domain}"
}

Write-Host ""
Write-Host "  LiteBin is running!" -ForegroundColor Green -BackgroundColor Black
Write-Host ""
Write-Host "  Dashboard:  " -NoNewline; Write-Host "$DashboardUrl" -ForegroundColor Cyan
Write-Host "  API:        " -NoNewline; Write-Host "$DashboardUrl (proxied via Caddy)" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Next steps:"
Write-Host "    1. Open the dashboard and create an admin account"
Write-Host "    2. Deploy apps using any of these methods:"
Write-Host "       a) GitHub Actions:  add a workflow that uses l8b-action"
Write-Host "       b) CLI:        curl -fsSL $L8B_IN | bash -s cli  then  l8b ship"
Write-Host "       c) Dashboard:  add from the web UI (only prebuilt images)"
Write-Host ""
Write-Host "  Manage LiteBin:  " -NoNewline; Write-Host "cd $InstallDir && docker compose logs -f" -ForegroundColor DarkGray
Write-Host ""
