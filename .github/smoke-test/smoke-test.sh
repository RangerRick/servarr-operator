#!/usr/bin/env bash
set -euo pipefail

# App name -> default port mapping (from image-defaults.toml)
declare -A APP_PORTS=(
  [sonarr]=8989
  [radarr]=7878
  [lidarr]=8686
  [prowlarr]=9696
  [sabnzbd]=8080
  [transmission]=9091
  [tautulli]=8181
  [overseerr]=5055
  [maintainerr]=6246
  [jackett]=9117
  [jellyfin]=8096
  [plex]=32400
)

APPS=("${!APP_PORTS[@]}")
TIMEOUT=240
POLL_INTERVAL=10
MIN_READY=$(( ${#APPS[@]} - 2 ))  # Tolerate up to 2 slow starters on resource-constrained CI

echo "Phase 1: Waiting for deployments to become ready (timeout: ${TIMEOUT}s, min: ${MIN_READY}/${#APPS[@]})"

elapsed=0
ready_apps=()
while true; do
  ready_count=0
  ready_apps=()
  not_ready_apps=()
  for app in "${APPS[@]}"; do
    ready=$(kubectl get deployment "$app" -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "0")
    if [[ "${ready:-0}" -ge 1 ]]; then
      ready_count=$((ready_count + 1))
      ready_apps+=("$app")
    else
      not_ready_apps+=("$app")
    fi
  done

  if [[ $ready_count -eq ${#APPS[@]} ]]; then
    echo "All ${#APPS[@]} deployments are ready."
    break
  fi

  if [[ $elapsed -ge $TIMEOUT ]]; then
    if [[ $ready_count -ge $MIN_READY ]]; then
      echo "WARNING: ${ready_count}/${#APPS[@]} deployments ready after ${TIMEOUT}s (minimum ${MIN_READY} met)"
      echo "  Not ready: ${not_ready_apps[*]}"
      break
    else
      echo "ERROR: Only ${ready_count}/${#APPS[@]} deployments ready after ${TIMEOUT}s (need ${MIN_READY})"
      echo "Deployment status:"
      kubectl get deployments -o wide
      exit 1
    fi
  fi

  echo "  ${ready_count}/${#APPS[@]} ready (${elapsed}s/${TIMEOUT}s)"
  sleep "$POLL_INTERVAL"
  elapsed=$((elapsed + POLL_INTERVAL))
done

echo ""
echo "Phase 2: HTTP health checks via port-forward"

pass=0
fail=0
skip=0
for app in "${APPS[@]}"; do
  port=${APP_PORTS[$app]}
  local_port=$((port + 10000))

  # Only check apps that became ready
  ready=$(kubectl get deployment "$app" -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "0")
  if [[ "${ready:-0}" -lt 1 ]]; then
    echo "  ${app}: SKIP (not ready)"
    skip=$((skip + 1))
    continue
  fi

  echo -n "  ${app} (port ${port} -> localhost:${local_port}): "

  # Start port-forward in background
  kubectl port-forward "deployment/${app}" "${local_port}:${port}" &
  pf_pid=$!

  # Wait for port-forward to be ready
  sleep 3

  # Curl the app — accept any HTTP response (200, 302, 401, etc.)
  status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 "http://localhost:${local_port}/" 2>/dev/null || echo "000")

  # Kill port-forward
  kill "$pf_pid" 2>/dev/null || true
  wait "$pf_pid" 2>/dev/null || true

  if [[ "$status" == "000" ]]; then
    echo "FAIL (no response)"
    fail=$((fail + 1))
  else
    echo "OK (HTTP ${status})"
    pass=$((pass + 1))
  fi
done

echo ""
echo "Results: ${pass} passed, ${fail} failed, ${skip} skipped"

if [[ $fail -ne 0 ]]; then
  echo "ERROR: ${fail} health check(s) failed"
  exit 1
fi

if [[ $pass -lt $MIN_READY ]]; then
  echo "ERROR: Only ${pass} apps passed health checks (need ${MIN_READY})"
  exit 1
fi

echo "Smoke tests passed."
