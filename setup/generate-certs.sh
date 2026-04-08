#!/usr/bin/env bash
set -euo pipefail

CERTS_DIR="./data/certs"
mkdir -p "$CERTS_DIR"

# Helper: prompt before overwriting
confirm_overwrite() {
    local file="$1"
    if [ -f "$file" ]; then
        read -r -p "File '$file' already exists. Overwrite? [y/N] " answer
        case "$answer" in
            [yY]*) return 0 ;;
            *) echo "Skipping $file"; return 1 ;;
        esac
    fi
    return 0
}

# Generate Root CA
generate_ca() {
    echo "==> Generating Root CA..."
    if confirm_overwrite "$CERTS_DIR/ca-key.pem"; then
        openssl genrsa -out "$CERTS_DIR/ca-key.pem" 4096
        chmod 600 "$CERTS_DIR/ca-key.pem"
        openssl req -new -x509 -days 3650 \
            -key "$CERTS_DIR/ca-key.pem" \
            -out "$CERTS_DIR/ca.pem" \
            -subj "/CN=LiteBin Root CA/O=LiteBin"
        echo "Root CA generated: $CERTS_DIR/ca.pem"
    fi
}

# Generate Master server cert
generate_master() {
    local master_host="${1:-localhost}"
    echo "==> Generating Master server cert (SAN: $master_host)..."
    if confirm_overwrite "$CERTS_DIR/server-key.pem"; then
        openssl genrsa -out "$CERTS_DIR/server-key.pem" 4096
        chmod 600 "$CERTS_DIR/server-key.pem"

        openssl req -new \
            -key "$CERTS_DIR/server-key.pem" \
            -out "$CERTS_DIR/server.csr" \
            -subj "/CN=$master_host/O=LiteBin Master"

        # Sign with CA, adding SAN (try IP+DNS first, fall back to DNS-only)
        openssl x509 -req -days 3650 \
            -in "$CERTS_DIR/server.csr" \
            -CA "$CERTS_DIR/ca.pem" \
            -CAkey "$CERTS_DIR/ca-key.pem" \
            -CAcreateserial \
            -out "$CERTS_DIR/server.pem" \
            -extfile <(printf "subjectAltName=DNS:%s,IP:%s" "$master_host" "$master_host") 2>/dev/null || \
        openssl x509 -req -days 3650 \
            -in "$CERTS_DIR/server.csr" \
            -CA "$CERTS_DIR/ca.pem" \
            -CAkey "$CERTS_DIR/ca-key.pem" \
            -CAcreateserial \
            -out "$CERTS_DIR/server.pem" \
            -extfile <(printf "subjectAltName=DNS:%s" "$master_host")

        rm -f "$CERTS_DIR/server.csr"
        echo "Master cert generated: $CERTS_DIR/server.pem"
    fi
}

# Generate per-node client cert
generate_node() {
    local node_name="$1"
    echo "==> Generating client cert for node: $node_name..."
    if confirm_overwrite "$CERTS_DIR/node-${node_name}-key.pem"; then
        openssl genrsa -out "$CERTS_DIR/node-${node_name}-key.pem" 4096
        chmod 600 "$CERTS_DIR/node-${node_name}-key.pem"

        openssl req -new \
            -key "$CERTS_DIR/node-${node_name}-key.pem" \
            -out "$CERTS_DIR/node-${node_name}.csr" \
            -subj "/CN=${node_name}/O=LiteBin Node"

        openssl x509 -req -days 3650 \
            -in "$CERTS_DIR/node-${node_name}.csr" \
            -CA "$CERTS_DIR/ca.pem" \
            -CAkey "$CERTS_DIR/ca-key.pem" \
            -CAcreateserial \
            -out "$CERTS_DIR/node-${node_name}.pem"

        rm -f "$CERTS_DIR/node-${node_name}.csr"
        echo "Node cert generated: $CERTS_DIR/node-${node_name}.pem"
    fi
}

# Main
case "${1:-}" in
    "")
        echo "Usage: $0 [ca|master <hostname>|node <node-name>|all <hostname>]"
        echo ""
        echo "Commands:"
        echo "  ca                    Generate Root CA"
        echo "  master <hostname>     Generate Master server cert"
        echo "  node <node-name>      Generate per-node client cert"
        echo "  all <hostname>        Generate CA + Master cert"
        exit 0
        ;;
    "ca")
        generate_ca
        ;;
    "master")
        generate_master "${2:-localhost}"
        ;;
    "node")
        if [ -z "${2:-}" ]; then
            echo "Error: node name required"
            exit 1
        fi
        generate_node "$2"
        ;;
    "all")
        generate_ca
        generate_master "${2:-localhost}"
        ;;
    *)
        # Treat first arg as node name for backward compat
        generate_node "$1"
        ;;
esac

echo "Done. Certs in $CERTS_DIR/"
