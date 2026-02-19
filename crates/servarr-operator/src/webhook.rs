use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use kube::Client;
use kube::api::{Api, ListParams};
use serde::{Deserialize, Serialize};
use servarr_crds::{AppConfig, AppType, ServarrApp, ServarrAppSpec};
use tracing::{debug, info, warn};

const DEFAULT_WEBHOOK_PORT: u16 = 9443;

/// Configuration for the webhook server.
#[derive(Clone)]
pub struct WebhookConfig {
    pub port: u16,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        let port = match std::env::var("WEBHOOK_PORT") {
            Ok(s) => match s.parse::<u16>() {
                Ok(p) => {
                    debug!(port = p, "using WEBHOOK_PORT from env");
                    p
                }
                Err(e) => {
                    warn!(value = %s, error = %e, "invalid WEBHOOK_PORT, using default {DEFAULT_WEBHOOK_PORT}");
                    DEFAULT_WEBHOOK_PORT
                }
            },
            Err(_) => DEFAULT_WEBHOOK_PORT,
        };
        Self { port }
    }
}

#[derive(Clone)]
struct WebhookState {
    client: Client,
}

// --- Admission API types ---

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionReview {
    api_version: String,
    kind: String,
    request: Option<AdmissionRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionRequest {
    uid: String,
    #[serde(default)]
    operation: String,
    #[serde(default)]
    namespace: String,
    object: serde_json::Value,
    #[serde(default)]
    old_object: Option<serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionReviewResponse {
    api_version: String,
    kind: String,
    response: AdmissionResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionResponse {
    uid: String,
    allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<AdmissionStatus>,
}

#[derive(Serialize)]
struct AdmissionStatus {
    message: String,
}

/// Start the validating webhook server.
///
/// Listens for `POST /validate-servarrapp` with AdmissionReview payloads.
/// TLS termination is expected to be handled externally (e.g. by a sidecar
/// or service mesh). Set `WEBHOOK_PORT` to override the default port 9443.
pub async fn run(config: WebhookConfig) -> anyhow::Result<()> {
    let client = Client::try_default().await?;
    let state = Arc::new(WebhookState { client });
    let app = Router::new()
        .route("/validate-servarrapp", post(validate_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!(%addr, "starting webhook server");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn validate_handler(
    State(_state): State<Arc<WebhookState>>,
    Json(review): Json<AdmissionReview>,
) -> impl IntoResponse {
    let request = match review.request {
        Some(req) => req,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing request"})),
            );
        }
    };

    let uid = request.uid.clone();
    let validation_result = validate_spec(
        &request.object,
        request.old_object.as_ref(),
        &request.operation,
        &request.namespace,
        &_state.client,
    )
    .await;

    let response = AdmissionReviewResponse {
        api_version: review.api_version,
        kind: review.kind,
        response: match validation_result {
            Ok(()) => AdmissionResponse {
                uid,
                allowed: true,
                status: None,
            },
            Err(msg) => {
                warn!(%msg, "admission rejected");
                AdmissionResponse {
                    uid,
                    allowed: false,
                    status: Some(AdmissionStatus { message: msg }),
                }
            }
        },
    };

    (
        StatusCode::OK,
        Json(serde_json::to_value(response).unwrap()),
    )
}

/// Validate a ServarrApp spec. Returns `Ok(())` on success or `Err(message)`.
async fn validate_spec(
    object: &serde_json::Value,
    old_object: Option<&serde_json::Value>,
    operation: &str,
    namespace: &str,
    client: &Client,
) -> Result<(), String> {
    let spec = object
        .get("spec")
        .ok_or_else(|| "missing spec field".to_string())?;

    let parsed: ServarrAppSpec =
        serde_json::from_value(spec.clone()).map_err(|e| format!("invalid spec: {e}"))?;

    debug!(
        operation,
        namespace,
        app = %parsed.app,
        instance = ?parsed.instance,
        "validating ServarrApp admission"
    );

    let mut errors = Vec::new();

    // Rule 1: AppConfig variant must match AppType
    validate_app_config_match(&parsed, &mut errors);

    // Rule 2: Port numbers must be in range 1-65535
    validate_port_ranges(&parsed, &mut errors);

    // Rule 3: Resource limits >= requests
    validate_resource_bounds(&parsed, &mut errors);

    // Rule 4: gateway.hosts must be non-empty when gateway.enabled
    validate_gateway_hosts(&parsed, &mut errors);

    // Rule 5: Volume names in persistence must be unique
    validate_unique_volume_names(&parsed, &mut errors);

    // Rule 6: Duplicate app+instance detection on CREATE
    if operation == "CREATE" && !namespace.is_empty() {
        validate_no_duplicate_instance(&parsed, namespace, client, &mut errors).await;
    }

    // Rule 6b: app and instance are immutable on UPDATE
    if operation == "UPDATE" {
        validate_identity_immutable(&parsed, old_object, &mut errors);
    }

    // Rule 7: Transmission settings must not override operator-managed keys
    validate_transmission_settings(&parsed, &mut errors);

    // Rule 8: Backup retention_count must be >= 1 when backups are enabled
    validate_backup_retention(&parsed, &mut errors);

    // Rule 9: IndexerDefinition names must be alphanumeric with optional hyphens
    validate_indexer_definition_names(&parsed, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn validate_identity_immutable(
    spec: &ServarrAppSpec,
    old_object: Option<&serde_json::Value>,
    errors: &mut Vec<String>,
) {
    let old_spec = old_object
        .and_then(|o| o.get("spec"))
        .and_then(|s| serde_json::from_value::<ServarrAppSpec>(s.clone()).ok());

    if let Some(old) = old_spec {
        if old.app != spec.app {
            debug!(
                old_app = %old.app,
                new_app = %spec.app,
                "rejecting app type change on UPDATE"
            );
            errors.push(format!(
                "spec.app is immutable (was '{}', got '{}')",
                old.app, spec.app
            ));
        }
        if old.instance != spec.instance {
            debug!(
                old_instance = ?old.instance,
                new_instance = ?spec.instance,
                "rejecting instance change on UPDATE"
            );
            errors.push(format!(
                "spec.instance is immutable (was {:?}, got {:?})",
                old.instance, spec.instance
            ));
        }
    }
}

fn validate_app_config_match(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref config) = spec.app_config {
        let valid = matches!(
            (&spec.app, config),
            (AppType::Transmission, AppConfig::Transmission(_))
                | (AppType::Sabnzbd, AppConfig::Sabnzbd(_))
                | (AppType::Prowlarr, AppConfig::Prowlarr(_))
        );
        if !valid {
            errors.push(format!(
                "appConfig variant does not match app type '{}'",
                spec.app
            ));
        }
    }
}

fn validate_port_ranges(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    let check_port = |port: i32, label: &str, errors: &mut Vec<String>| {
        if !(1..=65535).contains(&port) {
            errors.push(format!("{label}: port {port} out of range 1-65535"));
        }
    };

    if let Some(ref svc) = spec.service {
        for p in &svc.ports {
            check_port(p.port, &format!("service.ports[{}].port", p.name), errors);
            if let Some(cp) = p.container_port {
                check_port(
                    cp,
                    &format!("service.ports[{}].containerPort", p.name),
                    errors,
                );
            }
            if let Some(hp) = p.host_port {
                check_port(hp, &format!("service.ports[{}].hostPort", p.name), errors);
            }
        }
    }

    if let Some(AppConfig::Transmission(ref tc)) = spec.app_config
        && let Some(ref peer) = tc.peer_port
    {
        check_port(peer.port, "appConfig.transmission.peerPort.port", errors);
    }
}

fn validate_resource_bounds(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref res) = spec.resources {
        if let (Some(limit_val), Some(req_val)) =
            (parse_cpu(&res.limits.cpu), parse_cpu(&res.requests.cpu))
            && limit_val < req_val
        {
            errors.push(format!(
                "resources.limits.cpu ({}) must be >= resources.requests.cpu ({})",
                res.limits.cpu, res.requests.cpu
            ));
        }
        if let (Some(limit_val), Some(req_val)) = (
            parse_memory(&res.limits.memory),
            parse_memory(&res.requests.memory),
        ) && limit_val < req_val
        {
            errors.push(format!(
                "resources.limits.memory ({}) must be >= resources.requests.memory ({})",
                res.limits.memory, res.requests.memory
            ));
        }
    }
}

fn validate_gateway_hosts(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref gw) = spec.gateway
        && gw.enabled
        && gw.hosts.is_empty()
    {
        errors.push("gateway.hosts must be non-empty when gateway is enabled".into());
    }
}

async fn validate_no_duplicate_instance(
    spec: &ServarrAppSpec,
    namespace: &str,
    client: &Client,
    errors: &mut Vec<String>,
) {
    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let existing = match api.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "failed to list ServarrApps for duplicate check");
            return;
        }
    };

    let new_app_type = spec.app.to_string();
    let new_instance = spec.instance.as_deref().unwrap_or("");

    for app in &existing {
        let existing_app_type = app.spec.app.to_string();
        let existing_instance = app.spec.instance.as_deref().unwrap_or("");

        if existing_app_type == new_app_type && existing_instance == new_instance {
            let instance_desc = if new_instance.is_empty() {
                "(default)".to_string()
            } else {
                format!("'{new_instance}'")
            };
            errors.push(format!(
                "a ServarrApp with app={new_app_type} instance={instance_desc} already exists in namespace {namespace}"
            ));
            return;
        }
    }
}

fn validate_unique_volume_names(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref persistence) = spec.persistence {
        let mut seen = HashSet::new();
        for v in &persistence.volumes {
            if !seen.insert(&v.name) {
                errors.push(format!("duplicate volume name: '{}'", v.name));
            }
        }

        let mut nfs_seen = HashSet::new();
        for nfs in &persistence.nfs_mounts {
            if !nfs_seen.insert(&nfs.name) {
                errors.push(format!("duplicate nfsMount name: '{}'", nfs.name));
            }
        }
    }
}

/// Keys in Transmission settings.json that are managed by the operator and
/// must not be overridden via the raw `settings` field.
const TRANSMISSION_MANAGED_KEYS: &[&str] = &[
    "rpc-authentication-required",
    "rpc-username",
    "rpc-password",
    "rpc-bind-address",
    "peer-port",
    "peer-port-random-on-start",
    "peer-port-random-low",
    "peer-port-random-high",
    "watch-dir",
    "watch-dir-enabled",
];

fn validate_transmission_settings(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(AppConfig::Transmission(ref tc)) = spec.app_config
        && let serde_json::Value::Object(ref map) = tc.settings
    {
        for key in TRANSMISSION_MANAGED_KEYS {
            if map.contains_key(*key) {
                errors.push(format!(
                    "appConfig.transmission.settings must not contain operator-managed key '{key}'"
                ));
            }
        }
    }
}

fn validate_backup_retention(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(ref backup) = spec.backup
        && backup.enabled
        && backup.retention_count == 0
    {
        errors.push("backup.retentionCount must be >= 1 when backups are enabled".into());
    }
}

fn validate_indexer_definition_names(spec: &ServarrAppSpec, errors: &mut Vec<String>) {
    if let Some(AppConfig::Prowlarr(ref pc)) = spec.app_config {
        for def in &pc.custom_definitions {
            if !def
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
                || def.name.is_empty()
            {
                errors.push(format!(
                    "appConfig.prowlarr.customDefinitions[].name '{}' must be non-empty and contain only alphanumeric characters or hyphens",
                    def.name
                ));
            }
        }
    }
}

/// Parse CPU quantity to millicores for comparison.
fn parse_cpu(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    if let Some(m) = s.strip_suffix('m') {
        m.parse().ok()
    } else {
        s.parse::<f64>().ok().map(|v| (v * 1000.0) as u64)
    }
}

/// Parse memory quantity to bytes for comparison.
fn parse_memory(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    for (suffix, multiplier) in [
        ("Ti", 1024u64 * 1024 * 1024 * 1024),
        ("Gi", 1024 * 1024 * 1024),
        ("Mi", 1024 * 1024),
        ("Ki", 1024),
        ("T", 1000 * 1000 * 1000 * 1000),
        ("G", 1000 * 1000 * 1000),
        ("M", 1000 * 1000),
        ("K", 1000),
    ] {
        if let Some(num) = s.strip_suffix(suffix) {
            return num.parse::<u64>().ok().map(|v| v * multiplier);
        }
    }
    s.parse().ok()
}
