# install-windows.ps1
# Native PowerShell installer for LiteBin on Windows.
# Run (master): iex (irm https://l8b.in/windows.ps1)
# Run (CLI):    iex "& { $(irm https://l8b.in/windows.ps1) } -Cli"
#
# Flags:
#   -Cli                     Install the l8b CLI tool ONLY
#   -Master                  Install the master server (default)
#   -Domain <domain>         Override domain (default: localhost)
#   -DashboardSub <sub>      Dashboard subdomain (default: l8bin)
#   -InstallDir <path>       Install directory (default: ~/litebin)
#   -Clean                   Remove installed containers and files

param(
    [Parameter(Position = 0)]
    [ValidateSet("master", "cli", "agent", "certs", "update")]
    [string]$Component = "master",

    [string]$Domain = "localhost",
    [string]$DashboardSub = "l8bin",
    [string]$InstallDir = "",
    [switch]$Clean,
    [switch]$Cli,
    [switch]$Master,
    [switch]$Agent,
    [switch]$Certs,
    [switch]$Update,
    [switch]$SkipHosts
)

# Handle switches by setting $Component
if ($Cli) { $Component = "cli" }
elseif ($Master) { $Component = "master" }
elseif ($Agent) { $Component = "agent" }
elseif ($Certs) { $Component = "certs" }
elseif ($Update) { $Component = "update" }

$ErrorActionPreference = "Stop"
$Repo = "mtsandeep/l8bin"
$L8B_IN = "https://l8b.in"

# -- Detect if -Domain was explicitly passed (not default) ----------------------
# When PowerShell binds params, $PSBoundParameters contains only explicitly passed ones
$DomainWasExplicit = $PSBoundParameters.ContainsKey("Domain")
$isLocal = $Domain -eq "localhost"

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

# -- Helpers -----------------------------------------------------------------
function Get-LatestRelease {
    # Check for local release dir (env var or a 'release' folder next to the script)
    $ReleaseDir = if ($env:L8B_RELEASE_DIR) { $env:L8B_RELEASE_DIR } elseif (Test-Path "$PSScriptRoot\release") { "$PSScriptRoot\release" } else { $null }

    if ($ReleaseDir) {
        $global:Tag = "local"
        $global:ReleaseBase = $ReleaseDir
    } else {
        Write-Step "Fetching latest release from GitHub"
        try {
            $response = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{"User-Agent"="PowerShell"}
            $global:Tag = $response.tag_name
            if (-not $Tag) { throw "No tag found" }
        } catch {
            Write-Err "Could not fetch latest release from GitHub. Check your internet connection or GitHub repo."
            exit 1
        }
        Write-Info "Latest release: $Tag"
        $global:ReleaseBase = "https://github.com/$Repo/releases/download/$Tag"
    }
}

# -- CLI Install -------------------------------------------------------------
function Install-Cli {
    Get-LatestRelease

    $Binary = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "l8b-aarch64-windows.exe" } else { "l8b-x86_64-windows.exe" }
    
    $LocalBin = Join-Path $env:USERPROFILE ".local\bin"
    if (-not (Test-Path $LocalBin)) {
        New-Item -ItemType Directory -Path $LocalBin | Out-Null
    }
    
    $DestFile = Join-Path $LocalBin "l8b.exe"
    
    if ($Tag -eq "local") {
        Write-Step "Using local CLI binary..."
        $src = Join-Path $ReleaseBase $Binary
        if (-not (Test-Path $src)) { Write-Err "Local binary not found: $src"; exit 1 }
        Copy-Item $src $DestFile
    } else {
        Write-Step "Downloading l8b CLI ($Arch)..."
        Invoke-WebRequest -Uri "$ReleaseBase/$Binary" -OutFile $DestFile
    }

    Write-Info "Installed l8b.exe to $LocalBin"

    # Update PATH
    $CurrentPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($CurrentPath -notlike "*$LocalBin*") {
        Write-Step "Updating PATH"
        [Environment]::SetEnvironmentVariable("Path", $CurrentPath + ";" + $LocalBin, "User")
        $env:Path += ";" + $LocalBin
        Write-Info "Added $LocalBin to User PATH"
    } else {
        Write-Info "$LocalBin is already in PATH"
    }

    Write-Host ""
    Write-Info "Success! Restart your terminal or run this to refresh the current session:"
    Write-Host '  $env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")' -ForegroundColor Cyan
    Write-Host ""
}

# -- DNS / Hosts File helper -------------------------------------------------
function Update-HostsFile {
    param([string]$Hostname)
    
    $HostsPath = "$env:SystemRoot\System32\drivers\etc\hosts"
    $Entry = "127.0.0.1 $Hostname"
    
    if (Get-Content $HostsPath | Select-String -Pattern " $Hostname" -Quiet) {
        Write-Info "DNS: $Hostname already in hosts file."
        return
    }

    Write-Step "Adding $Hostname to hosts file (requires Admin)"
    try {
        Add-Content -Path $HostsPath -Value "`n$Entry" -ErrorAction Stop
        Write-Info "DNS: Added $Entry to $HostsPath"
    } catch {
        Write-Warning "DNS: Failed to update hosts file. Please add manually: $Entry"
    }
}

# -- Master Install ----------------------------------------------------------
function Install-Master {
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
    $TargetDir = if ($InstallDir) { $InstallDir } else { Join-Path $env:USERPROFILE "litebin" }

    # -- Check for existing installation -----------------------------------------
    if ((Test-Path "$TargetDir\docker-compose.yml") -and (-not $Clean)) {
        $running = docker compose -f "$TargetDir\docker-compose.yml" ps --format "{{.Names}}" 2>$null
        if ($running) {
            Write-Host ""
            Write-Host "  Warning: LiteBin is already running in $TargetDir" -ForegroundColor Yellow
            $reply = Read-Host "  Continue and reinstall? [y/N]"
            if ($reply -notmatch '^[Yy]') {
                Write-Info "Cancelled."
                return
            }
        }
    }

    $OrchDir = Join-Path $TargetDir "orchestrator"
    $DashDir = Join-Path $TargetDir "dashboard"
    $ProjectsDir = Join-Path $TargetDir "projects"

    Get-LatestRelease

    # -- Create directories ------------------------------------------------------
    foreach ($d in @($TargetDir, $OrchDir, $DashDir, $ProjectsDir)) {
        New-Item -ItemType Directory -Force -Path $d | Out-Null
    }

    # -- Orchestrator binary -----------------------------------------------------
    $orchFile = Join-Path $OrchDir "litebin-orchestrator"

    if ($Tag -eq "local") {
        $orchSrc = Join-Path $ReleaseBase "litebin-orchestrator-$Arch-linux"
        if (Test-Path $orchSrc) {
            Write-Step "Using local orchestrator ($Arch)"
            Copy-Item $orchSrc $orchFile
        } else {
            Write-Step "No local Linux orchestrator found. Downloading ($Arch)"
            Invoke-WebRequest -Uri "https://github.com/mtsandeep/l8bin/releases/latest/download/litebin-orchestrator-$Arch-linux" -OutFile $orchFile
        }
    } else {
        Write-Step "Downloading orchestrator ($Arch)"
        Invoke-WebRequest -Uri "$ReleaseBase/litebin-orchestrator-$Arch-linux" -OutFile $orchFile
    }
    $orchMB = [math]::Round((Get-Item $orchFile).Length / 1MB, 1)
    Write-Info "  litebin-orchestrator-$Arch-linux  (${orchMB} MB)"

    # -- Dashboard --------------------------------------------------------------
    $dashDist = Join-Path $DashDir "dist"
    New-Item -ItemType Directory -Force -Path $dashDist | Out-Null

    if ($Tag -eq "local") {
        $dashZipSrc = Join-Path $ReleaseBase "l8b-dashboard.zip"
        if (Test-Path $dashZipSrc) {
            Write-Step "Using local dashboard"
            Expand-Archive -Path $dashZipSrc -DestinationPath $DashDir -Force
        } else {
            Write-Step "No local dashboard zip found. Downloading latest"
            $dashZipTmp = Join-Path $TargetDir "l8b-dashboard.zip"
            Invoke-WebRequest -Uri "https://github.com/mtsandeep/l8bin/releases/latest/download/l8b-dashboard.zip" -OutFile $dashZipTmp
            Expand-Archive -Path $dashZipTmp -DestinationPath $DashDir -Force
            Remove-Item $dashZipTmp -ErrorAction SilentlyContinue
        }
    } else {
        Write-Step "Downloading dashboard"
        $dashZip = Join-Path $TargetDir "l8b-dashboard.zip"
        Invoke-WebRequest -Uri "$ReleaseBase/l8b-dashboard.zip" -OutFile $dashZip
        Expand-Archive -Path $dashZip -DestinationPath $DashDir -Force
        Remove-Item $dashZip -ErrorAction SilentlyContinue
    }
    $dashKB = [math]::Round((Get-ChildItem $dashDist -Recurse | Measure-Object -Property Length -Sum).Sum / 1KB, 1)
    Write-Info "  l8b-dashboard-dist  (${dashKB} KB)"

    # -- Generate orchestrator Dockerfile ---------------------------------------
    Set-Content -Path (Join-Path $OrchDir "Dockerfile") -Value @'
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY litebin-orchestrator /app/litebin-orchestrator
RUN chmod +x /app/litebin-orchestrator
WORKDIR /app
RUN mkdir -p /app/data
CMD ["/app/litebin-orchestrator"]
'@

    # -- Generate dashboard Dockerfile ------------------------------------------
    Set-Content -Path (Join-Path $DashDir "Dockerfile") -Value @'
FROM nginx:alpine
COPY dist/ /usr/share/nginx/html/
EXPOSE 80
'@

    # -- Generate .env ----------------------------------------------------------
    $Timestamp = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    Set-Content -Path (Join-Path $TargetDir ".env") -Value @"
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

    # -- DNS Setup --------------------------------------------------------------
    if ($isLocal -and -not $SkipHosts) {
        Update-HostsFile "${DashboardSub}.localhost"
    }

    # -- Generate Caddyfile (conditional on local vs live) ----------------------
    if ($isLocal) {
        # Local: listen on localhost + subdomain.localhost (no TLS)
        $caddyfile = @'
{
	admin 0.0.0.0:2019
}

http://{$DASHBOARD_SUBDOMAIN}.{$DOMAIN}, http://localhost, http://127.0.0.1 {
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
'@
    } else {
        # Live: Caddy auto-provisions HTTPS via Let's Encrypt
        $caddyfile = @'
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
'@
    }
    Set-Content -Path (Join-Path $TargetDir "Caddyfile") -Value $caddyfile

    # -- Generate docker-compose.yml --------------------------------------------
    $orchPorts = if ($isLocal) {
        @'
    ports:
      - "5080:5080"
'@
    } else {
        ""
    }

    $compose = @"
services:
  orchestrator:
    build: ./orchestrator
    container_name: litebin-orchestrator
    restart: unless-stopped
$orchPorts    volumes:
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
    Set-Content -Path (Join-Path $TargetDir "docker-compose.yml") -Value $compose

    # -- Start ------------------------------------------------------------------
    Write-Step "Starting LiteBin"
    Push-Location $TargetDir
    docker compose up -d --build
    Pop-Location

    # -- Done -------------------------------------------------------------------
    $DashboardUrl = if ($Domain -eq "localhost") { "http://localhost (or https://${DashboardSub}.localhost)" } else { "https://${DashboardSub}.${Domain}" }

    Write-Host ""
    Write-Host "  LiteBin is running!" -ForegroundColor Green
    Write-Host ""
    Write-Host "  Dashboard:  $DashboardUrl" -ForegroundColor Cyan
    if ($Domain -eq "localhost") {
        Write-Host "  API (Local-only fallback): http://localhost:5080" -ForegroundColor DarkGray
    }
    Write-Host ""
    Write-Host "  Next steps:"
    Write-Host "    1. Open the dashboard and create an admin account"
    Write-Host "    2. Login via CLI:"
    if ($Domain -eq "localhost") {
        Write-Host "       l8b login --server http://localhost:5080" -ForegroundColor Cyan
    } else {
        Write-Host "       l8b login --server https://${DashboardSub}.${Domain}" -ForegroundColor Cyan
    }
    Write-Host ""
    Write-Host "  Manage: cd $TargetDir && docker compose logs -f" -ForegroundColor DarkGray
    Write-Host ""
}

# -- Execution ---------------------------------------------------------------
if ($Clean) {
    Write-Step "Cleaning up"
    $dir = if ($InstallDir) { $InstallDir } else { Join-Path $env:USERPROFILE "litebin" }
    if (Test-Path "$dir\docker-compose.yml") {
        Write-Info "Stopping containers in $dir..."
        Push-Location $dir
        # Use --remove-orphans and -v (volumes), ignore noise on stderr
        & docker compose down -v --remove-orphans 2>$null
        Pop-Location
    }
    if (Test-Path $dir) {
        Write-Info "Removing directory $dir..."
        Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
    }
    Write-Info "Done."
    exit 0
}

# Banner
Write-Host "  LiteBin for Windows" -ForegroundColor Magenta -BackgroundColor Black
Write-Host ""

# -- Interactive: Local or Live? (only for master install, if -Domain not explicit)
if ($Component -eq "master" -and -not $DomainWasExplicit) {
    Write-Host "  Is this a local development setup or a live server?" -ForegroundColor White
    $choice = Read-Host "  [L]ocal (default) / Live"
    if ($choice -match '^[Ll]$' -or $choice -eq '') {
        $Domain = "localhost"
        $isLocal = $true
    } else {
        $liveDomain = Read-Host "  Enter your domain (e.g. l8bin.example.com)"
        if ($liveDomain -and $liveDomain -ne '') {
            $Domain = $liveDomain.Trim()
        } else {
            Write-Err "Domain is required for live setup."
            exit 1
        }
        $isLocal = $false
    }
    Write-Host ""
}

$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }

switch ($Component) {
    "master" { Install-Master }
    "cli"    { Install-Cli }
    default  { Write-Err "Unknown component: $Component" }
}
