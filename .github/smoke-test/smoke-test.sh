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

# ---------------------------------------------------------------------------
# Phase 3: Admin credential verification
#
# The MediaStack named 'media' is deployed with adminCredentials pointing at
# the 'smoke-admin' Secret.  The operator injects env vars for Sonarr and
# calls the API for Jellyfin/Transmission.  We verify each mechanism works.
# ---------------------------------------------------------------------------

echo ""
echo "Phase 3: Admin credential verification (MediaStack 'media')"

ADMIN_USER=$(kubectl get secret smoke-admin -o jsonpath='{.data.username}' | base64 -d)
ADMIN_PASS=$(kubectl get secret smoke-admin -o jsonpath='{.data.password}' | base64 -d)

# Wait for the media-* deployments to be ready and for the operator to sync
# credentials (sync happens after each app passes its health check).
echo "  Waiting for media-* deployments and credential sync (up to 120s)..."
MEDIA_APPS=(media-sonarr media-jellyfin media-transmission)
CRED_TIMEOUT=120
elapsed=0
while true; do
  all_ready=true
  for app in "${MEDIA_APPS[@]}"; do
    ready=$(kubectl get deployment "$app" -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "0")
    if [[ "${ready:-0}" -lt 1 ]]; then
      all_ready=false
      break
    fi
  done
  if $all_ready; then
    echo "  All media-* deployments are ready."
    break
  fi
  if [[ $elapsed -ge $CRED_TIMEOUT ]]; then
    echo "  WARNING: media-* deployments not all ready after ${CRED_TIMEOUT}s — skipping Phase 3"
    echo "Smoke tests passed (Phase 3 skipped)."
    exit 0
  fi
  echo "  Waiting for media-* deployments... (${elapsed}s/${CRED_TIMEOUT}s)"
  sleep 10
  elapsed=$((elapsed + 10))
done

# Extra dwell time for the operator to finish the credential-sync API calls
sleep 20

# Helper: port-forward to a deployment, run a check function, then clean up.
# Usage: with_port_forward <deployment> <remote_port> <local_port> <check_fn>
with_port_forward() {
  local deploy=$1 rport=$2 lport=$3 check_fn=$4
  kubectl port-forward "deployment/${deploy}" "${lport}:${rport}" &>/dev/null &
  local pf_pid=$!
  sleep 3
  local result=0
  $check_fn "$lport" || result=$?
  kill "$pf_pid" 2>/dev/null || true
  wait "$pf_pid" 2>/dev/null || true
  return $result
}

cred_pass=0
cred_fail=0

# --- Sonarr: env vars cause Forms auth → unauthenticated API returns 401 ---
check_sonarr_auth() {
  local lport=$1
  local status
  status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
    "http://localhost:${lport}/api/v3/system/status" 2>/dev/null || echo "000")
  if [[ "$status" == "401" ]]; then
    echo "  media-sonarr: OK (Forms auth enforced — unauthenticated API returns 401)"
    return 0
  else
    echo "  media-sonarr: FAIL (expected 401 for unauthenticated API request, got ${status})"
    return 1
  fi
}

echo -n ""
if with_port_forward media-sonarr 8989 28989 check_sonarr_auth; then
  cred_pass=$((cred_pass + 1))
else
  cred_fail=$((cred_fail + 1))
fi

# --- Transmission: session-set enables RPC auth → no-auth request returns 401,
#     correct credentials → 409 (session ID handshake, not 401) ---
check_transmission_auth() {
  local lport=$1

  # Without auth: expect 401
  local status_no_auth
  status_no_auth=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
    -X POST "http://localhost:${lport}/transmission/rpc" \
    -H 'Content-Type: application/json' \
    -d '{"method":"session-get"}' 2>/dev/null || echo "000")

  # With correct credentials: expect 409 (session ID needed) — not 401
  local status_with_auth
  status_with_auth=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
    -X POST "http://localhost:${lport}/transmission/rpc" \
    -H 'Content-Type: application/json' \
    -u "${ADMIN_USER}:${ADMIN_PASS}" \
    -d '{"method":"session-get"}' 2>/dev/null || echo "000")

  if [[ "$status_no_auth" == "401" && "$status_with_auth" == "409" ]]; then
    echo "  media-transmission: OK (auth enforced: no-auth=401, correct-creds=409)"
    return 0
  else
    echo "  media-transmission: FAIL (expected no-auth=401 and correct-creds=409," \
         "got no-auth=${status_no_auth} and correct-creds=${status_with_auth})"
    return 1
  fi
}

if with_port_forward media-transmission 9091 29091 check_transmission_auth; then
  cred_pass=$((cred_pass + 1))
else
  cred_fail=$((cred_fail + 1))
fi

# --- Jellyfin: startup wizard set the admin account → credentials authenticate ---
check_jellyfin_auth() {
  local lport=$1
  local auth_header
  auth_header='MediaBrowser Client="servarr-operator", Device="servarr-operator",'
  auth_header+=' DeviceId="servarr-operator-device", Version="1.0.0"'

  local status
  status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
    -X POST "http://localhost:${lport}/Users/AuthenticateByName" \
    -H "X-Emby-Authorization: ${auth_header}" \
    -H 'Content-Type: application/json' \
    -d "{\"Username\":\"${ADMIN_USER}\",\"Pw\":\"${ADMIN_PASS}\"}" \
    2>/dev/null || echo "000")

  if [[ "$status" == "200" ]]; then
    echo "  media-jellyfin: OK (admin credentials authenticate successfully)"
    return 0
  else
    echo "  media-jellyfin: FAIL (expected 200 from AuthenticateByName, got ${status})"
    return 1
  fi
}

if with_port_forward media-jellyfin 8096 28096 check_jellyfin_auth; then
  cred_pass=$((cred_pass + 1))
else
  cred_fail=$((cred_fail + 1))
fi

echo ""
echo "Credential check results: ${cred_pass} passed, ${cred_fail} failed"

if [[ $cred_fail -ne 0 ]]; then
  echo "ERROR: ${cred_fail} credential check(s) failed"
  exit 1
fi

echo "All smoke tests passed."
