#!/usr/bin/env bash
set -euo pipefail

# Generate CRD YAML from the operator binary and split into per-CRD files
# for the servarr-crds Helm chart.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CRD_CHART_DIR="$REPO_ROOT/charts/servarr-crds/templates"

mkdir -p "$CRD_CHART_DIR"

# Generate all CRDs into a temp file
TMPFILE=$(mktemp)
trap 'rm -f "$TMPFILE"' EXIT

cargo run -p servarr-operator -- crd 2>/dev/null > "$TMPFILE"

# The output contains two CRDs concatenated without --- separators.
# Split on each "apiVersion:" line that starts a new document.
awk '
/^apiVersion:/ { n++ }
n == 1 { print > "/tmp/servarr-crd-1.yaml" }
n == 2 { print > "/tmp/servarr-crd-2.yaml" }
' "$TMPFILE"

SERVARRAPP_CRD="$CRD_CHART_DIR/servarrapp-crd.yaml"
MEDIASTACK_CRD="$CRD_CHART_DIR/mediastack-crd.yaml"

for f in /tmp/servarr-crd-1.yaml /tmp/servarr-crd-2.yaml; do
    [ -s "$f" ] || continue
    name=$(grep -m1 '^  name:' "$f" | awk '{print $2}')
    case "$name" in
        servarrapps.servarr.dev)
            cp -f "$f" "$SERVARRAPP_CRD"
            echo "Generated servarrapp-crd.yaml"
            ;;
        mediastacks.servarr.dev)
            cp -f "$f" "$MEDIASTACK_CRD"
            echo "Generated mediastack-crd.yaml"
            ;;
        *)
            echo "Warning: unknown CRD '$name'" >&2
            ;;
    esac
    rm -f "$f"
done

echo "CRD generation complete."
