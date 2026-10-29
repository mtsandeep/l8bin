# install-windows.ps1
# Native PowerShell installer for LiteBin on Windows.
# Run (master): iex (irm https://l8b.in/windows.ps1)
# Run (CLI):    iex "& { $(irm https://l8b.in/windows.ps1) } -Cli"
# Run (update): iex "& { $(irm https://l8b.in/windows.ps1) } -Update"
#
# Flags:
#   -Cli                     Install the l8b CLI tool ONLY
#   -Master                  Install the master server (default)
#   -Update                  Update an existing master installation
#   -Domain <domain>         Override domain (default: localhost)
#   -DashboardSub <sub>      Dashboard subdomain (default: l8bin)
#   -InstallDir <path>       Install directory (default: ~/litebin)
#   -Clean                   Remove installed containers and files
#
# agent and certs are Linux-only (see install.sh).

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

if ($Cli) { $Component = "cli" }
elseif ($Master) { $Component = "master" }
elseif ($Agent) { $Component = "agent" }
elseif ($Certs) { $Component = "certs" }
elseif ($Update) { $Component = "update" }

$ErrorActionPreference = "Stop"
$Repo = "mtsandeep/l8bin"
$L8B_IN = "https://l8b.in"

# -- Detect if -Domain was explicitly passed (not default) ----------------------
$DomainWasExplicit = $PSBoundParameters.ContainsKey("Domain")

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

function Normalize-ReleaseTag([string]$Value) {
    $tag = $Value.Trim()
    if (-not $tag) { return "" }
    if ($tag -match '^[vV]') { $tag = $tag.Substring(1) }
    return "v$tag"
}

function Confirm-ReleaseExists([string]$Tag) {
    if ($env:L8B_RELEASE_DIR -or (Test-Path "$PSScriptRoot\release")) { return }
    try {
        $null = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/$Tag" -Headers @{"User-Agent"="PowerShell"}
    } catch {
        Write-Err "Release $Tag not found. Check available releases at https://github.com/$Repo/releases"
        exit 1
    }
}

function Save-VerifiedDownload([string]$Url, [string]$Dest, [string]$Name) {
    if ($global:Tag -eq "local") {
        $src = Join-Path $global:ReleaseBase (Split-Path $Url -Leaf)
        if (-not (Test-Path $src)) { Write-Err "Local release file not found: $src"; exit 1 }
        Write-Step "Using local $Name"
        Copy-Item $src $Dest -Force
        return
    }

    Write-Step "Downloading $Name"
    Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing

    try {
        $resp = Invoke-WebRequest -Uri (($Url -replace '/[^/]+$', '/checksums.txt')) -UseBasicParsing
        $checksums = if ($resp.Content -is [byte[]]) { [Text.Encoding]::UTF8.GetString($resp.Content) } else { [string]$resp.Content }
        $asset = Split-Path $Url -Leaf
        # checksums.txt keys assets with a platform dir prefix (e.g. "windows/l8b.exe")
        $line = ($checksums -split "`n" | Where-Object { $_ -match ('[/\s]' + [regex]::Escape($asset) + '\s*$') } | Select-Object -First 1)
        if ($line) {
            $expected = ($line -split '\s+')[0]
            $actual = (Get-FileHash -Path $Dest -Algorithm SHA256).Hash
            if ($expected -and ($actual -ne $expected)) {
                Write-Err "Checksum mismatch for $Name! Expected: $expected, Got: $actual"
                exit 1
            }
            Write-Info "Checksum verified."
        }
    } catch {
        # checksums unavailable
    }
}

function Read-InstallEnv([string]$Path) {
    $values = @{}
    if (Test-Path $Path) {
        foreach ($line in Get-Content $Path) {
            if ($line -match '^\s*([A-Za-z_][A-Za-z0-9_]*)=(.*)$') {
                $values[$Matches[1]] = $Matches[2].Trim()
            }
        }
    }
    return $values
}

function Test-ContentChanged([string]$Path, [string]$NewContent) {
    if (-not (Test-Path $Path)) { return $true }
    $existing = ([string](Get-Content $Path -Raw) -replace "`r`n", "`n").TrimEnd("`n")
    $new = ([string]$NewContent -replace "`r`n", "`n").TrimEnd("`n")
    return ($existing -ne $new)
}

function Show-FileDiff([string]$Path, [string]$NewContent) {
    $old = @(if (Test-Path $Path) { Get-Content $Path | Where-Object { $_ -ne "" } })
    $new = @(($NewContent -split "`r?`n") | Where-Object { $_ -ne "" })
    Compare-Object -ReferenceObject $old -DifferenceObject $new | ForEach-Object {
        $mark = if ($_.SideIndicator -eq "<=") { "-" } else { "+" }
        Write-Host "  $mark $($_.InputObject)"
    }
}

function Invoke-ComposeUp([string]$Dir) {
    Push-Location $Dir
    try {
        docker compose up -d --build
        if ($LASTEXITCODE -ne 0) {
            Write-Err "docker compose up failed. Then re-run: cd $Dir && docker compose up -d --build"
            exit 1
        }
    } finally { Pop-Location }
}

# -- Generated files -----------------------------------------------------------
function Write-OrchestratorDockerfile([string]$OrchDir) {
    Set-Content -Path (Join-Path $OrchDir "Dockerfile") -Value @'
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
ENV _RJEM_MALLOC_CONF=dirty_decay_ms:1000,muzzy_decay_ms:0,narenas:2
COPY litebin-orchestrator /app/litebin-orchestrator
RUN chmod +x /app/litebin-orchestrator
WORKDIR /app
RUN mkdir -p /app/data
CMD ["/app/litebin-orchestrator"]
'@
}

function New-ComposeContent([bool]$IsLocal) {
    $orchPorts = if ($IsLocal) { "    ports:`n      - `"5080:5080`"`n" } else { "" }

    return @"
services:
  orchestrator:
    build: ./orchestrator
    container_name: litebin-orchestrator
    restart: unless-stopped
    logging:
      driver: json-file
      options:
        max-size: "10m"
        max-file: "3"
$orchPorts    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - orchestrator-data:/app/data
      - ./projects:/app/projects
      - ./exports:/app/data/exports
      - ./imports:/app/data/imports
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
    logging:
      driver: json-file
      options:
        max-size: "5m"
        max-file: "2"
    networks:
      - litebin-network

  caddy:
    image: caddy:2.11.2-alpine
    container_name: litebin-caddy
    restart: unless-stopped
    logging:
      driver: json-file
      options:
        max-size: "25m"
        max-file: "3"
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
}

function New-ComposeOverrideContent {
    return @'
services:
  orchestrator:
    oom_score_adj: -300
    cpu_shares: 26214
    mem_reservation: 28m

  dashboard:
    oom_score_adj: -300
    cpu_shares: 26214
    mem_reservation: 8m

  caddy:
    oom_score_adj: -300
    cpu_shares: 26214
    mem_reservation: 30m
'@
}

function New-CaddyfileContent([bool]$IsLocal) {
    # single-quoted to keep the {$VAR} Caddy env placeholders literal
    $routes = @'
	# All dashboard-host traffic (API + SPA) goes to the orchestrator,
	# which routes API paths itself and proxies the rest to the dashboard.
	reverse_proxy litebin-orchestrator:5080
'@
    $sites = if ($IsLocal) {
        # Local: plain HTTP
        'http://{$DASHBOARD_SUBDOMAIN}.{$DOMAIN}, http://localhost, http://127.0.0.1 {'
    } else {
        # Live: Caddy auto-provisions HTTPS via Let's Encrypt
        '{$DASHBOARD_SUBDOMAIN}.{$DOMAIN} {'
    }
    return "{`n`tadmin 0.0.0.0:2019`n}`n`n" + $sites + "`n" + $routes + "`n}`n"
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

    Save-VerifiedDownload -Url "$ReleaseBase/$Binary" -Dest $DestFile -Name "l8b CLI ($Arch)"

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

    $OrchDir = Join-Path $TargetDir "orchestrator"
    $DashDir = Join-Path $TargetDir "dashboard"
    $ProjectsDir = Join-Path $TargetDir "projects"
    $ExportsDir = Join-Path $TargetDir "exports"
    $ImportsDir = Join-Path $TargetDir "imports"

    # An existing running installation goes to the update flow
    if ((Test-Path "$TargetDir\docker-compose.yml") -and (-not $Clean)) {
        $running = docker compose -f "$TargetDir\docker-compose.yml" ps --format "{{.Names}}" 2>$null
        if ($running) {
            Update-Master
            return
        }
    }

    Get-LatestRelease

    # -- Create directories ------------------------------------------------------
    foreach ($d in @($TargetDir, $OrchDir, $DashDir, $ProjectsDir, $ExportsDir, $ImportsDir)) {
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
            Invoke-WebRequest -Uri "https://github.com/mtsandeep/l8bin/releases/latest/download/litebin-orchestrator-$Arch-linux" -OutFile $orchFile -UseBasicParsing
        }
    } else {
        Save-VerifiedDownload -Url "$ReleaseBase/litebin-orchestrator-$Arch-linux" -Dest $orchFile -Name "orchestrator ($Arch)"
    }
    $orchMB = [math]::Round((Get-Item $orchFile).Length / 1MB, 1)
    Write-Info "  litebin-orchestrator-$Arch-linux  (${orchMB} MB)"

    # -- Dashboard --------------------------------------------------------------
    New-Item -ItemType Directory -Force -Path $DashDir | Out-Null

    if ($Tag -eq "local") {
        $dashZipSrc = Join-Path $ReleaseBase "l8b-dashboard.zip"
        if (Test-Path $dashZipSrc) {
            Write-Step "Using local dashboard"
            Expand-Archive -Path $dashZipSrc -DestinationPath $DashDir -Force
        } else {
            Write-Step "No local dashboard zip found. Downloading latest"
            $dashZipTmp = Join-Path $TargetDir "l8b-dashboard.zip"
            Invoke-WebRequest -Uri "https://github.com/mtsandeep/l8bin/releases/latest/download/l8b-dashboard.zip" -OutFile $dashZipTmp -UseBasicParsing
            Expand-Archive -Path $dashZipTmp -DestinationPath $DashDir -Force
            Remove-Item $dashZipTmp -ErrorAction SilentlyContinue
        }
    } else {
        $dashZip = Join-Path $TargetDir "l8b-dashboard.zip"
        Save-VerifiedDownload -Url "$ReleaseBase/l8b-dashboard.zip" -Dest $dashZip -Name "dashboard"
        Expand-Archive -Path $dashZip -DestinationPath $DashDir -Force
        Remove-Item $dashZip -ErrorAction SilentlyContinue
    }

    $dashDist = Join-Path $DashDir "dist"
    $dashKB = [math]::Round((Get-ChildItem $dashDist -Recurse | Measure-Object -Property Length -Sum).Sum / 1KB, 1)
    Write-Info "  l8b-dashboard-dist  (${dashKB} KB)"

    # -- Generate orchestrator Dockerfile ---------------------------------------
    Write-OrchestratorDockerfile $OrchDir

    # -- Generate .env -----------------------------------------------------------
    $EnvFile = Join-Path $TargetDir ".env"
    if (Test-Path $EnvFile) {
        Write-Info "Preserving existing .env"
    } else {
        $Timestamp = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
        Set-Content -Path $EnvFile -Value @"
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
HOST=[::]
PORT=5080

# Sleep / Janitor
DEFAULT_AUTO_STOP_MINS=15
JANITOR_INTERVAL_SECS=300
FLUSH_INTERVAL_SECS=60

# Routing
ROUTING_MODE=master_proxy
"@
    }

    # -- DNS Setup --------------------------------------------------------------
    if ($isLocal -and -not $SkipHosts) {
        Update-HostsFile "${DashboardSub}.localhost"
    }

    # -- Generate Caddyfile -----------------------------------------------------
    Set-Content -Path (Join-Path $TargetDir "Caddyfile") -Value (New-CaddyfileContent $isLocal)

    # -- Generate docker-compose.yml --------------------------------------------
    Set-Content -Path (Join-Path $TargetDir "docker-compose.yml") -Value (New-ComposeContent $isLocal)
    Set-Content -Path (Join-Path $TargetDir "docker-compose.override.yml") -Value (New-ComposeOverrideContent)

    # -- Start ------------------------------------------------------------------
    Write-Step "Starting LiteBin"
    Invoke-ComposeUp $TargetDir

    Write-Info "Waiting for services to start..."
    Start-Sleep -Seconds 3
    if ((docker inspect -f '{{.State.Running}}' litebin-orchestrator 2>$null) -ne "true") {
        Write-Err "Orchestrator did not start. Check: cd $TargetDir && docker compose logs orchestrator"
        exit 1
    }

    # Save installed version
    Set-Content -Path (Join-Path $TargetDir ".version") -Value $Tag
    # -- Done -------------------------------------------------------------------
    $DashboardUrl = if ($isLocal) { "http://${DashboardSub}.localhost" } else { "https://${DashboardSub}.${Domain}" }
    $LoginUrl = if ($isLocal) { "http://localhost:5080" } else { $DashboardUrl }

    Write-Host ""
    Write-Host "  LiteBin is running!" -ForegroundColor Green
    Write-Host ""
    Write-Host "  Dashboard:  $DashboardUrl" -ForegroundColor Cyan
    if ($isLocal) {
        Write-Host "  API (direct fallback): http://localhost:5080" -ForegroundColor DarkGray
    }
    Write-Host ""
    Write-Host "  Next steps:"
    Write-Host "    1. Open the dashboard and create an admin account"
    Write-Host "    2. Login via CLI:"
    Write-Host "       l8b login --server $LoginUrl" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "  Manage: cd $TargetDir && docker compose logs -f --tail 100 orchestrator" -ForegroundColor DarkGray
    Write-Host ""
}

# -- Master Update -------------------------------------------------------------
function Update-Master {
    $TargetDir = if ($InstallDir) { $InstallDir } else { Join-Path $env:USERPROFILE "litebin" }
    if (-not (Test-Path "$TargetDir\docker-compose.yml")) {
        Write-Err "No LiteBin master installation found in $TargetDir. Run the installer without -Update to install first."
        exit 1
    }

    if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
        Write-Err "Docker Desktop is required."
        exit 1
    }

    $VersionFile = Join-Path $TargetDir ".version"
    $CurrentVersion = "unknown"
    if (Test-Path $VersionFile) { $CurrentVersion = ([string](Get-Content $VersionFile -First 1)).Trim() }

    # Local vs live comes from the install's own .env
    $InstallEnv = Read-InstallEnv (Join-Path $TargetDir ".env")
    $EnvDomain = if ($InstallEnv["DOMAIN"]) { $InstallEnv["DOMAIN"] } else { "localhost" }
    $LocalInstall = $EnvDomain -eq "localhost"

    Get-LatestRelease

    Write-Host ""
    Write-Host "  Installed version: $CurrentVersion" -ForegroundColor Cyan
    Write-Host "  Latest version:    $Tag" -ForegroundColor Cyan
    Write-Host ""

    $TargetTag = $Tag
    if ($CurrentVersion -eq $Tag) {
        $reply = Read-Host "  Already up to date. Reinstall? [y/N]"
        if ($reply -notmatch '^[Yy]') { Write-Info "Cancelled."; return }
    } else {
        Write-Host "  Options:"
        Write-Host "    1) Update to $Tag (latest)"
        Write-Host "    2) Enter a specific version"
        Write-Host ""
        $choice = Read-Host "  Choose [1]"
        if ($choice -eq "2") {
            $ver = Read-Host "  Enter version (e.g. v0.1.2)"
            $TargetTag = Normalize-ReleaseTag $ver
            if (-not $TargetTag) { Write-Err "Version is required"; exit 1 }
            Confirm-ReleaseExists $TargetTag
            $global:ReleaseBase = "https://github.com/$Repo/releases/download/$TargetTag"
        }
    }

    Write-Host ""
    if ($CurrentVersion -eq $TargetTag) {
        $reply = Read-Host "  Reinstall $CurrentVersion? [y/N]"
        if ($reply -notmatch '^[Yy]') { Write-Info "Cancelled."; return }
    } else {
        $reply = Read-Host "  Update from $CurrentVersion to $TargetTag? [Y/n]"
        if ($reply -match '^[Nn]') { Write-Info "Cancelled."; return }
    }

    # Export/import bundle folders (bind targets) exist regardless of whether
    # the compose diff below is applied.
    foreach ($d in @((Join-Path $TargetDir "exports"), (Join-Path $TargetDir "imports"))) {
        New-Item -ItemType Directory -Force -Path $d | Out-Null
    }

    $newCompose = New-ComposeContent -IsLocal $LocalInstall
    $composePath = Join-Path $TargetDir "docker-compose.yml"
    if (Test-ContentChanged $composePath $newCompose) {
        Write-Host ""
        Write-Host "  docker-compose.yml needs to be updated in this version:" -ForegroundColor Yellow
        Write-Host ""
        Write-Host "  - removed, + added:" -ForegroundColor DarkGray
        Show-FileDiff $composePath $newCompose
        Write-Host ""
        $reply = Read-Host "  Apply these changes? [y/N]"
        if ($reply -match '^[Yy]') {
            Set-Content -Path $composePath -Value $newCompose
            Write-Info "docker-compose.yml updated"
        } else {
            Write-Info "Skipped - continuing with the current docker-compose.yml. Features needing the change above may not work until it is applied (re-run the update to retry)."
        }
    }

    $newOverride = New-ComposeOverrideContent
    $overridePath = Join-Path $TargetDir "docker-compose.override.yml"
    if (Test-ContentChanged $overridePath $newOverride) {
        Write-Host ""
        Write-Host "  docker-compose.override.yml needs to be updated in this version:" -ForegroundColor Yellow
        Write-Host ""
        Write-Host "  - removed, + added:" -ForegroundColor DarkGray
        Show-FileDiff $overridePath $newOverride
        Write-Host ""
        $reply = Read-Host "  Apply these changes? [y/N]"
        if ($reply -match '^[Yy]') {
            Set-Content -Path $overridePath -Value $newOverride
            Write-Info "docker-compose.override.yml updated"
        } else {
            Write-Info "Skipped - resource protections unchanged until this file is updated."
        }
    }

    $newCaddy = New-CaddyfileContent -IsLocal $LocalInstall
    $caddyPath = Join-Path $TargetDir "Caddyfile"
    if (Test-ContentChanged $caddyPath $newCaddy) {
        Write-Host ""
        Write-Host "  Caddyfile updated for this version:" -ForegroundColor Yellow
        Show-FileDiff $caddyPath $newCaddy
        Set-Content -Path $caddyPath -Value $newCaddy
        Write-Info "Caddyfile refreshed - caddy picks it up on its next restart"
    }

    Write-Info "Backing up database..."
    $backupOk = $false
    if ((docker inspect -f '{{.State.Running}}' litebin-orchestrator 2>$null) -eq "true") {
        $BackupDir = Join-Path $TargetDir "backups"
        New-Item -ItemType Directory -Force -Path $BackupDir | Out-Null
        $BackupFile = Join-Path $BackupDir ("litebin.db." + (Get-Date -Format "yyyyMMddHHmmss"))
        docker cp litebin-orchestrator:/app/data/litebin.db $BackupFile 2>$null
        if ($LASTEXITCODE -eq 0) {
            Write-Info "Database backed up to $BackupFile"
            $backupOk = $true
        }
    }
    if (-not $backupOk) {
        Write-Warning "Could not backup database (orchestrator not running or no data yet)"
    }

    $OrchDir = Join-Path $TargetDir "orchestrator"
    New-Item -ItemType Directory -Force -Path $OrchDir | Out-Null
    Save-VerifiedDownload -Url "$ReleaseBase/litebin-orchestrator-$Arch-linux" -Dest (Join-Path $OrchDir "litebin-orchestrator") -Name "orchestrator $TargetTag ($Arch)"

    Write-OrchestratorDockerfile $OrchDir

    $DashDir = Join-Path $TargetDir "dashboard"
    New-Item -ItemType Directory -Force -Path $DashDir | Out-Null
    Remove-Item (Join-Path $DashDir "dist") -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item (Join-Path $DashDir "nginx.conf") -Force -ErrorAction SilentlyContinue
    Remove-Item (Join-Path $DashDir "Dockerfile") -Force -ErrorAction SilentlyContinue
    $dashZip = Join-Path $TargetDir "l8b-dashboard.zip"
    Save-VerifiedDownload -Url "$ReleaseBase/l8b-dashboard.zip" -Dest $dashZip -Name "dashboard $TargetTag"
    Expand-Archive -Path $dashZip -DestinationPath $DashDir -Force
    Remove-Item $dashZip -ErrorAction SilentlyContinue

    Write-Host ""
    $reply = Read-Host "  Restart LiteBin now? [y/N]"
    if ($reply -notmatch '^[Yy]') {
        Set-Content -Path $VersionFile -Value $TargetTag
        Write-Info "Update staged - the running version is unchanged until you restart:"
        Write-Host "    cd $TargetDir && docker compose up -d --build" -ForegroundColor DarkGray
        return
    }

    Write-Info "Restarting LiteBin..."
    $RestartEpoch = [DateTime]::UtcNow
    Invoke-ComposeUp $TargetDir

    Write-Info "Waiting for services to start..."
    Start-Sleep -Seconds 3

    $running = docker inspect -f '{{.State.Running}}' litebin-orchestrator 2>$null
    $created = docker inspect -f '{{.Created}}' litebin-orchestrator 2>$null
    $createdAfter = $false
    if ($created) {
        try {
            $createdDt = [DateTime]::Parse($created, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind)
            $createdAfter = ($createdDt -gt $RestartEpoch)
        } catch { $createdAfter = $false }
    }

    if ($running -eq "true" -and $createdAfter) {
        Set-Content -Path $VersionFile -Value $TargetTag
        Write-Host ""
        Write-Host "  LiteBin $TargetTag is running!" -ForegroundColor Green
        Write-Host ""
        Write-Host "  Manage: cd $TargetDir && docker compose logs -f --tail 100 orchestrator" -ForegroundColor DarkGray
    } elseif ($running -eq "true" -and $CurrentVersion -eq $TargetTag) {
        Set-Content -Path $VersionFile -Value $TargetTag
        Write-Info "No changes to apply - orchestrator already running $TargetTag."
    } else {
        Write-Err "Orchestrator was not recreated - the update did not apply. Re-run: cd $TargetDir && docker compose up -d --build (logs: docker compose logs orchestrator)"
        exit 1
    }
}

# -- Interactive menu -----------------------------------------------------------
function Show-Menu {
    $dir = if ($InstallDir) { $InstallDir } else { Join-Path $env:USERPROFILE "litebin" }
    $state = "not installed"
    if (Test-Path "$dir\docker-compose.yml") {
        $state = "installed"
        if (Test-Path "$dir\.version") {
            $v = ([string](Get-Content "$dir\.version" -First 1)).Trim()
            if ($v) { $state = "installed  $v" }
        }
    }
    Write-Host "  This host:"
    if ($state -eq "not installed") {
        Write-Host "    Master:  not installed" -ForegroundColor DarkGray
    } else {
        Write-Host "    Master:  $state  ($dir)" -ForegroundColor Green
    }
    Write-Host ""
    Write-Host "  What would you like to do?"
    Write-Host ""
    Write-Host "    1) Master server (orchestrator + dashboard + Caddy)"
    Write-Host "    2) CLI only (l8b deploy tool)"
    Write-Host "    3) Update LiteBin"
    Write-Host ""
    $choice = Read-Host "  Choose [1]"
    switch ($choice) {
        "2" { return "cli" }
        "3" { return "update" }
        default { return "master" }
    }
}

# -- Execution ---------------------------------------------------------------
if ($Clean) {
    Write-Step "Cleaning up"
    $dir = if ($InstallDir) { $InstallDir } else { Join-Path $env:USERPROFILE "litebin" }
    if (Test-Path "$dir\docker-compose.yml") {
        Write-Info "Stopping containers in $dir..."
        Push-Location $dir
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

if ($PSBoundParameters.Count -eq 0) {
    $Component = Show-Menu
    Write-Host ""
}

# -- Interactive: domain (only for master install, if -Domain not explicit) ----
if ($Component -eq "master" -and -not $DomainWasExplicit) {
    $choice = Read-Host "  Domain name (e.g. example.com) [localhost]"
    if ($choice -and $choice.Trim()) { $Domain = $choice.Trim() }
    Write-Host ""
}
$isLocal = $Domain -eq "localhost"

$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }

switch ($Component) {
    "master" { Install-Master }
    "cli"    { Install-Cli }
    "update" { Update-Master }
    "agent"  { Write-Err "agent is Linux-only. Use install.sh on a Linux server: curl -fsSL $L8B_IN | sudo bash -s agent"; exit 1 }
    "certs"  { Write-Err "certs management is not supported by the Windows installer. Manage certs from a Linux master: curl -fsSL $L8B_IN | sudo bash -s certs"; exit 1 }
    default  { Write-Err "Unknown component: $Component"; exit 1 }
}
