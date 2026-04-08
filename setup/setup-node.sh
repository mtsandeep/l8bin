#!/usr/bin/env bash
set -euo pipefail

# DEPRECATED: Use 'curl -sSL l8b.in | sh -s agent' instead.
# This script is kept for backward compatibility.

# LiteBin Agent Node Setup Script
# Usage: sudo ./setup-node.sh <node-name> [certs-source-dir]

NODE_NAME="${1:-litebin-node}"
CERTS_SRC="${2:-./data/certs}"
AGENT_IMAGE="${AGENT_IMAGE:-ghcr.io/litebin/litebin-agent:latest}"

CERTS_DEST="/etc/litebin/certs"

echo "==> Setting up LiteBin Agent node: $NODE_NAME"

# 1. Install Docker Engine if absent
if ! command -v docker &>/dev/null; then
    echo "==> Installing Docker Engine..."
    curl -fsSL https://get.docker.com | sh
else
    echo "==> Docker already installed: $(docker --version)"
fi

# 2. Create litebin user and add to docker group
if ! id litebin &>/dev/null; then
    echo "==> Creating litebin user..."
    useradd -r -s /bin/false litebin
fi
usermod -aG docker litebin
echo "==> litebin user added to docker group"

# 3. Configure UFW
if command -v ufw &>/dev/null; then
    echo "==> Configuring UFW..."
    ufw --force reset
    ufw default deny incoming
    ufw default allow outgoing
    ufw allow 80/tcp
    ufw allow 443/tcp
    ufw allow 5083/tcp
    ufw --force enable
    echo "==> UFW configured"
else
    echo "WARNING: UFW not found, skipping firewall configuration"
fi

# 4. Copy certs
echo "==> Copying certificates to $CERTS_DEST..."
mkdir -p "$CERTS_DEST"
cp "$CERTS_SRC/ca.pem" "$CERTS_DEST/ca.pem"
cp "$CERTS_SRC/node-${NODE_NAME}.pem" "$CERTS_DEST/agent.pem"
cp "$CERTS_SRC/node-${NODE_NAME}-key.pem" "$CERTS_DEST/agent-key.pem"
chmod 600 "$CERTS_DEST/agent-key.pem"
chown -R litebin:litebin "$CERTS_DEST"
echo "==> Certificates installed"

# 5. Pull and run litebin-agent container
echo "==> Pulling litebin-agent image..."
docker pull "$AGENT_IMAGE"

# Stop and remove existing container if present
docker stop litebin-agent 2>/dev/null || true
docker rm litebin-agent 2>/dev/null || true

echo "==> Starting litebin-agent container..."
docker run -d \
    --name litebin-agent \
    --restart unless-stopped \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v "$CERTS_DEST":/certs:ro \
    -v litebin-agent-data:/etc/litebin/data \
    -p 5083:8443 \
    -e AGENT_PORT=8443 \
    -e AGENT_CERT_PATH=/certs/agent.pem \
    -e AGENT_KEY_PATH=/certs/agent-key.pem \
    -e AGENT_CA_CERT_PATH=/certs/ca.pem \
    "$AGENT_IMAGE"

echo "==> litebin-agent started"
echo ""
echo "Node setup complete!"
echo "  Node name: $NODE_NAME"
echo "  Agent port: 5083"
echo ""
echo "Next steps:"
echo "  1. Add this node in the dashboard (Nodes → Add Node)"
echo "  2. Click 'Connect' in the dashboard to register the agent"
