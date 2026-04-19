#!/usr/bin/env bash
# build.sh
# Automates local build and prepares 'release' folder for testing the installer on Linux/macOS.
set -euo pipefail

# Find repository root (one level up from /setup)
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

detect_platform() {
  case "$(uname -s)" in
    Linux)  echo "linux" ;;
    Darwin) echo "macos" ;;
    *)      echo "unknown" ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64)  echo "x86_64" ;;
    aarch64|arm64) echo "aarch64" ;;
    *)             echo "unknown" ;;
  esac
}

PLATFORM=$(detect_platform)
ARCH=$(detect_arch)

if [ "$PLATFORM" = "unknown" ] || [ "$ARCH" = "unknown" ]; then
    echo "Unsupported OS or Architecture: $(uname -s) / $(uname -m)"
    exit 1
fi

echo -e "\033[0;36mBuilding LiteBin components in release mode...\033[0m"
cargo build --release

RELEASE_DIR="${ROOT_DIR}/release"
mkdir -p "$RELEASE_DIR"

# Map of internal names to desired release names
# format: [internal_bin_name]="target_release_prefix"
declare -A BINARIES=(
    ["l8b"]="l8b"
    ["litebin-agent"]="litebin-agent"
    ["litebin-orchestrator"]="litebin-orchestrator"
)

echo -e "\n\033[0;36mPreparing 'release' folder for installer...\033[0m"

for bin in "${!BINARIES[@]}"; do
    SRC="${ROOT_DIR}/target/release/${bin}"
    PREFIX="${BINARIES[$bin]}"
    
    # Check for .exe on Windows (though this script is for bash, good to be safe)
    if [ ! -f "$SRC" ] && [ -f "${SRC}.exe" ]; then
        SRC="${SRC}.exe"
    fi

    if [ -f "$SRC" ]; then
        # Naming patterns for Linux/macOS
        if [ "$bin" == "l8b" ]; then
            # CLI uses platform suffix: l8b-x86_64-linux
            DEST="${RELEASE_DIR}/${PREFIX}-${ARCH}-${PLATFORM}"
        else
            # Master components are always linux: litebin-orchestrator-x86_64-linux
            DEST="${RELEASE_DIR}/${PREFIX}-${ARCH}-linux"
        fi
        
        cp "$SRC" "$DEST"
        chmod +x "$DEST"
        echo -e "  \033[0;32m[OK]\033[0m ${SRC} -> ${DEST}"
    else
        echo -e "  \033[0;2m[SKIP]\033[0m ${SRC} not found."
    fi
done

echo -e "\n\033[1;37mDone! You can now test the installer locally:\033[0m"
echo -e "\033[0;32mcurl -fsSL https://l8b.in | L8B_RELEASE_DIR=./release bash -s cli\033[0m"
