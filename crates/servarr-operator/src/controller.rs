use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, PersistentVolumeClaim, Secret, Service};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Event, EventType, Recorder};
use kube::runtime::watcher;
use kube::{Client, CustomResourceExt, Resource, ResourceExt};
use servarr_api::AppKind;
use servarr_crds::{AppType, Condition, ServarrApp, ServarrAppStatus, condition_types};
use thiserror::Error;
use tokio::time::Duration;
use tracing::{error, info, warn};

use crate::context::Context;
use crate::metrics::{
    increment_backup_operations, increment_drift_corrections, increment_reconcile_total,
    observe_reconcile_duration, set_managed_apps,
};

fn app_type_to_kind(app_type: &AppType) -> AppKind {
    match app_type {
        AppType::Sonarr => AppKind::Sonarr,
        AppType::Radarr => AppKind::Radarr,
        AppType::Lidarr => AppKind::Lidarr,
        AppType::Prowlarr => AppKind::Prowlarr,
        other => panic!("AppKind not supported for {other:?}"),
    }
}

const FIELD_MANAGER: &str = "servarr-operator";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Kubernetes API error: {0}")]
    Kube(#[source] kube::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
}

pub fn print_crd() -> Result<()> {
    let crd = ServarrApp::crd();
    let yaml = serde_yaml::to_string(&crd)?;
    println!("{yaml}");
    Ok(())
}

pub async fn run(server_state: crate::server::ServerState) -> Result<()> {
    let client = Client::try_default().await?;
    let ctx = Arc::new(Context::new(client.clone()));

    let (apps, deployments, services, config_maps) = if let Some(ref ns) = ctx.watch_namespace {
        (
            Api::<ServarrApp>::namespaced(client.clone(), ns),
            Api::<Deployment>::namespaced(client.clone(), ns),
            Api::<Service>::namespaced(client.clone(), ns),
            Api::<ConfigMap>::namespaced(client.clone(), ns),
        )
    } else {
        (
            Api::<ServarrApp>::all(client.clone()),
            Api::<Deployment>::all(client.clone()),
            Api::<Service>::all(client.clone()),
            Api::<ConfigMap>::all(client.clone()),
        )
    };

    info!("Starting Servarr Operator controller");
    server_state.set_ready();

    Controller::new(apps, watcher::Config::default())
        .owns(deployments, watcher::Config::default())
        .owns(services, watcher::Config::default())
        .owns(config_maps, watcher::Config::default())
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "reconciled"),
                Err(e) => error!(%e, "reconcile error"),
            }
        })
        .await;

    Ok(())
}

async fn reconcile(app: Arc<ServarrApp>, ctx: Arc<Context>) -> Result<Action, Error> {
    let client = &ctx.client;
    let name = app.name_any();
    let ns = app.namespace().unwrap_or_else(|| "default".into());
    let pp = PatchParams::apply(FIELD_MANAGER).force();

    let recorder = Recorder::new(client.clone(), ctx.reporter.clone());
    let obj_ref = app.object_ref(&());

    info!(%name, %ns, app_type = %app.spec.app, "reconciling");

    let app_type = app.spec.app.as_str();
    let start_time = std::time::Instant::now();

    // Prowlarr cleanup finalizer for Servarr v3 apps
    const PROWLARR_FINALIZER: &str = "servarr.dev/prowlarr-sync";
    const OVERSEERR_FINALIZER: &str = "servarr.dev/overseerr-sync";
    if matches!(
        app.spec.app,
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr
    ) {
        if app.metadata.deletion_timestamp.is_some() {
            // App is being deleted — clean up Prowlarr registration
            if let Err(e) =
                cleanup_prowlarr_registration(client, &app, &ns, &recorder, &obj_ref).await
            {
                warn!(%name, error = %e, "failed to clean up Prowlarr registration");
            }
            // App is being deleted — clean up Overseerr registration
            if let Err(e) =
                cleanup_overseerr_registration(client, &app, &ns, &recorder, &obj_ref).await
            {
                warn!(%name, error = %e, "failed to clean up Overseerr registration");
            }
            // Remove finalizers
            let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);
            let finalizers: Vec<String> = app
                .metadata
                .finalizers
                .as_ref()
                .map(|f| {
                    f.iter()
                        .filter(|x| *x != PROWLARR_FINALIZER && *x != OVERSEERR_FINALIZER)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let patch = serde_json::json!({
                "metadata": { "finalizers": finalizers }
            });
            sa_api
                .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
                .await
                .map_err(Error::Kube)?;
            return Ok(Action::await_change());
        }

        // Ensure finalizer is present if a Prowlarr with sync enabled exists
        let has_prowlarr_finalizer = app
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|f| f.contains(&PROWLARR_FINALIZER.to_string()));
        if !has_prowlarr_finalizer && prowlarr_sync_exists(client, &ns).await {
            let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);
            let mut finalizers = app.metadata.finalizers.clone().unwrap_or_default();
            finalizers.push(PROWLARR_FINALIZER.to_string());
            let patch = serde_json::json!({
                "metadata": { "finalizers": finalizers }
            });
            sa_api
                .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
                .await
                .map_err(Error::Kube)?;
        }

        // Ensure Overseerr finalizer is present if an Overseerr with sync enabled exists
        let has_overseerr_finalizer = app
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|f| f.contains(&OVERSEERR_FINALIZER.to_string()));
        if !has_overseerr_finalizer && overseerr_sync_exists(client, &ns).await {
            let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);
            let mut finalizers = app.metadata.finalizers.clone().unwrap_or_default();
            finalizers.push(OVERSEERR_FINALIZER.to_string());
            let patch = serde_json::json!({
                "metadata": { "finalizers": finalizers }
            });
            sa_api
                .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
                .await
                .map_err(Error::Kube)?;
        }
    }

    // Check for restore-from-backup annotation
    if let Some(restore_id) = app
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("servarr.dev/restore-from"))
        .cloned()
    {
        maybe_restore_backup(client, &app, &ns, &name, &restore_id, &recorder, &obj_ref).await;
    }

    // Build and apply Deployment
    let deployment = servarr_resources::deployment::build(&app, &ctx.image_overrides);
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), &ns);
    deploy_api
        .patch(&name, &pp, &Patch::Apply(&deployment))
        .await
        .map_err(Error::Kube)?;

    // Check for drift: read back the Deployment and compare only operator-managed fields.
    // Kubernetes adds default fields (terminationGracePeriodSeconds, dnsPolicy, etc.)
    // so we check that our desired fields are a subset of the actual state.
    let applied_deploy = deploy_api.get(&name).await.map_err(Error::Kube)?;
    if let (Some(desired_spec), Some(actual_spec)) =
        (deployment.spec.as_ref(), applied_deploy.spec.as_ref())
    {
        let desired_json = serde_json::to_value(&desired_spec.template).unwrap_or_default();
        let actual_json = serde_json::to_value(&actual_spec.template).unwrap_or_default();
        if !json_is_subset(&desired_json, &actual_json) {
            let diff = json_diff_paths(&desired_json, &actual_json, "".to_string());
            warn!(%name, "deployment drift detected, re-applying");
            tracing::debug!(%name, ?diff, "drift details");
            recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "DriftDetected".into(),
                        note: Some("Deployment pod template differs from desired state".into()),
                        action: "DriftCheck".into(),
                        secondary: None,
                    },
                    &obj_ref,
                )
                .await
                .map_err(Error::Kube)?;
            increment_drift_corrections(app_type, &ns, "Deployment");
            // Re-apply to correct drift
            deploy_api
                .patch(&name, &pp, &Patch::Apply(&deployment))
                .await
                .map_err(Error::Kube)?;
        }
    }

    // Build and apply Service
    let service = servarr_resources::service::build(&app);
    let svc_api = Api::<Service>::namespaced(client.clone(), &ns);
    svc_api
        .patch(&name, &pp, &Patch::Apply(&service))
        .await
        .map_err(Error::Kube)?;

    // Build and apply PVCs (get-or-create to avoid mutating immutable fields)
    let pvcs = servarr_resources::pvc::build_all(&app);
    let pvc_api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), &ns);
    for pvc in &pvcs {
        let pvc_name = pvc.metadata.name.as_deref().unwrap_or("unknown");
        match pvc_api.get(pvc_name).await {
            Ok(_) => {
                // PVC exists, don't modify (immutable fields)
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                pvc_api
                    .patch(pvc_name, &pp, &Patch::Apply(pvc))
                    .await
                    .map_err(Error::Kube)?;
            }
            Err(e) => return Err(Error::Kube(e)),
        }
    }

    // Build and apply NetworkPolicy.
    // Enabled when: network_policy_config is set (takes precedence), or the
    // boolean network_policy flag is true (default).
    let has_explicit_config = app.spec.network_policy_config.is_some();
    let network_policy_enabled = has_explicit_config || app.spec.network_policy.unwrap_or(true);
    if has_explicit_config && app.spec.network_policy == Some(false) {
        tracing::debug!(
            app = %name,
            "network_policy_config is set; overriding network_policy=false"
        );
    }
    if network_policy_enabled {
        let np = servarr_resources::networkpolicy::build(&app);
        let np_api = Api::<NetworkPolicy>::namespaced(client.clone(), &ns);
        np_api
            .patch(&name, &pp, &Patch::Apply(&np))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply ConfigMap (Transmission settings, SABnzbd whitelist)
    if let Some(cm) = servarr_resources::configmap::build(&app) {
        let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
        let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
        cm_api
            .patch(cm_name, &pp, &Patch::Apply(&cm))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply tar-unpack ConfigMap (SABnzbd)
    if let Some(cm) = servarr_resources::configmap::build_tar_unpack(&app) {
        let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
        let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
        cm_api
            .patch(cm_name, &pp, &Patch::Apply(&cm))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply Prowlarr custom definitions ConfigMap
    if let Some(cm) = servarr_resources::configmap::build_prowlarr_definitions(&app) {
        let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
        let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
        cm_api
            .patch(cm_name, &pp, &Patch::Apply(&cm))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply SSH bastion authorized-keys Secret
    if let Some(secret) = servarr_resources::secret::build_authorized_keys(&app) {
        let secret_name = secret.metadata.name.as_deref().unwrap_or(&name);
        let secret_api = Api::<Secret>::namespaced(client.clone(), &ns);
        secret_api
            .patch(secret_name, &pp, &Patch::Apply(&secret))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply SSH bastion restricted-rsync ConfigMap
    if let Some(cm) = servarr_resources::configmap::build_ssh_bastion_restricted_rsync(&app) {
        let cm_name = cm.metadata.name.as_deref().unwrap_or(&name);
        let cm_api = Api::<ConfigMap>::namespaced(client.clone(), &ns);
        cm_api
            .patch(cm_name, &pp, &Patch::Apply(&cm))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply HTTPRoute or TCPRoute (if gateway enabled)
    // Gateway API types use DynamicObject since they're not in k8s-openapi
    if let Some(route) = servarr_resources::tcproute::build(&app) {
        // TCPRoute takes precedence when route_type is Tcp or TLS is enabled
        let api_resource = kube::discovery::ApiResource {
            group: "gateway.networking.k8s.io".into(),
            version: "v1alpha2".into(),
            api_version: "gateway.networking.k8s.io/v1alpha2".into(),
            kind: "TCPRoute".into(),
            plural: "tcproutes".into(),
        };
        let route_api =
            Api::<kube::api::DynamicObject>::namespaced_with(client.clone(), &ns, &api_resource);
        let route_data = serde_json::to_value(&route).map_err(Error::Serialization)?;
        route_api
            .patch(&name, &pp, &Patch::Apply(route_data))
            .await
            .map_err(Error::Kube)?;
    } else if let Some(route) = servarr_resources::httproute::build(&app) {
        let api_resource = kube::discovery::ApiResource {
            group: "gateway.networking.k8s.io".into(),
            version: "v1".into(),
            api_version: "gateway.networking.k8s.io/v1".into(),
            kind: "HTTPRoute".into(),
            plural: "httproutes".into(),
        };
        let route_api =
            Api::<kube::api::DynamicObject>::namespaced_with(client.clone(), &ns, &api_resource);
        let route_data = serde_json::to_value(&route).map_err(Error::Serialization)?;
        route_api
            .patch(&name, &pp, &Patch::Apply(route_data))
            .await
            .map_err(Error::Kube)?;
    }

    // Build and apply cert-manager Certificate (if TLS is enabled)
    if let Some(cert) = servarr_resources::certificate::build(&app) {
        let api_resource = kube::discovery::ApiResource {
            group: "cert-manager.io".into(),
            version: "v1".into(),
            api_version: "cert-manager.io/v1".into(),
            kind: "Certificate".into(),
            plural: "certificates".into(),
        };
        let cert_api =
            Api::<kube::api::DynamicObject>::namespaced_with(client.clone(), &ns, &api_resource);
        let cert_data = serde_json::to_value(&cert).map_err(Error::Serialization)?;
        cert_api
            .patch(&name, &pp, &Patch::Apply(cert_data))
            .await
            .map_err(Error::Kube)?;
    }

    // API health check and update check (non-blocking)
    let (health_condition, update_condition) = check_api_health(client, &app, &ns).await;

    // Backup scheduling (non-blocking)
    let backup_status = maybe_run_backup(client, &app, &ns, &recorder, &obj_ref).await;

    // Prowlarr cross-app sync (only for Prowlarr-type apps with sync enabled)
    if app.spec.app == AppType::Prowlarr
        && let Some(ref sync_spec) = app.spec.prowlarr_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        if let Err(e) = sync_prowlarr_apps(client, &app, target_ns, &recorder, &obj_ref).await {
            warn!(%name, error = %e, "Prowlarr sync failed");
        }
    }

    // Overseerr cross-app sync (only for Overseerr-type apps with sync enabled)
    if app.spec.app == AppType::Overseerr
        && let Some(ref sync_spec) = app.spec.overseerr_sync
        && sync_spec.enabled
    {
        let target_ns = sync_spec.namespace_scope.as_deref().unwrap_or(&ns);
        if let Err(e) = sync_overseerr_servers(client, &app, target_ns, &recorder, &obj_ref).await {
            warn!(%name, error = %e, "Overseerr sync failed");
        }
    }

    // Update status
    update_status(
        client,
        &app,
        &ns,
        &name,
        health_condition,
        update_condition,
        backup_status,
    )
    .await?;

    info!(%name, "reconciliation complete");

    let duration = start_time.elapsed().as_secs_f64();
    observe_reconcile_duration(app_type, duration);
    increment_reconcile_total(app_type, "success");

    // Update managed-apps gauge from informer cache
    let gauge_api = if let Some(ref ns) = ctx.watch_namespace {
        Api::<ServarrApp>::namespaced(client.clone(), ns)
    } else {
        Api::<ServarrApp>::all(client.clone())
    };
    if let Ok(app_list) = gauge_api.list(&kube::api::ListParams::default()).await {
        let mut counts: std::collections::HashMap<(String, String), i64> =
            std::collections::HashMap::new();
        for a in &app_list.items {
            let key = (
                a.spec.app.as_str().to_owned(),
                a.namespace().unwrap_or_default(),
            );
            *counts.entry(key).or_default() += 1;
        }
        for ((t, n), count) in &counts {
            set_managed_apps(t, n, *count);
        }
    }

    recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "ReconcileSuccess".into(),
                note: Some(format!("All resources reconciled in {duration:.2}s")),
                action: "Reconcile".into(),
                secondary: None,
            },
            &obj_ref,
        )
        .await
        .map_err(Error::Kube)?;

    Ok(Action::requeue(Duration::from_secs(300)))
}

async fn check_api_health(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
) -> (Option<Condition>, Option<Condition>) {
    let _health_check = match app.spec.api_health_check.as_ref() {
        Some(hc) if hc.enabled => hc,
        _ => return (None, None),
    };
    let secret_name = match app.spec.api_key_secret.as_deref() {
        Some(s) => s,
        None => return (None, None),
    };

    let now = chrono_now();
    let api_key = match servarr_api::read_secret_key(client, ns, secret_name, "api-key").await {
        Ok(k) => k,
        Err(e) => {
            warn!(error = %e, "failed to read API key secret");
            let cond = Condition {
                condition_type: condition_types::APP_HEALTHY.to_string(),
                status: "Unknown".to_string(),
                reason: "SecretReadError".to_string(),
                message: e.to_string(),
                last_transition_time: now,
            };
            return (Some(cond), None);
        }
    };

    let app_name = servarr_resources::common::app_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let base_url = format!("http://{app_name}.{ns}.svc:{port}");

    use servarr_api::HealthCheck;
    let (healthy, update_cond): (Result<bool, String>, Option<Condition>) = match app.spec.app {
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr | AppType::Prowlarr => {
            match servarr_api::ServarrClient::new(
                &base_url,
                &api_key,
                app_type_to_kind(&app.spec.app),
            ) {
                Ok(c) => {
                    let h = c.is_healthy().await.map_err(|e| e.to_string());
                    let uc = check_update_available(&c, &now).await;
                    (h, uc)
                }
                Err(e) => (Err(e.to_string()), None),
            }
        }
        AppType::Sabnzbd => match servarr_api::SabnzbdClient::new(&base_url, &api_key) {
            Ok(c) => {
                let h = c.is_healthy().await.map_err(|e| e.to_string());
                (h, None)
            }
            Err(e) => (Err(e.to_string()), None),
        },
        AppType::Transmission => {
            match servarr_api::TransmissionClient::new(&base_url, None, None) {
                Ok(c) => {
                    let h = c.is_healthy().await.map_err(|e| e.to_string());
                    (h, None)
                }
                Err(e) => (Err(e.to_string()), None),
            }
        }
        AppType::Jellyfin => match servarr_api::JellyfinClient::new(&base_url) {
            Ok(c) => {
                let h = c.is_healthy().await.map_err(|e| e.to_string());
                (h, None)
            }
            Err(e) => (Err(e.to_string()), None),
        },
        AppType::Plex => match servarr_api::PlexClient::new(&base_url) {
            Ok(c) => {
                let h = c.is_healthy().await.map_err(|e| e.to_string());
                (h, None)
            }
            Err(e) => (Err(e.to_string()), None),
        },
        _ => return (None, None),
    };

    let health_cond = match healthy {
        Ok(true) => Condition::ok(
            condition_types::APP_HEALTHY,
            "Healthy",
            "API responded healthy",
            &now,
        ),
        Ok(false) => Condition::fail(
            condition_types::APP_HEALTHY,
            "Unhealthy",
            "API responded unhealthy",
            &now,
        ),
        Err(msg) => Condition {
            condition_type: condition_types::APP_HEALTHY.to_string(),
            status: "Unknown".to_string(),
            reason: "ApiError".to_string(),
            message: msg,
            last_transition_time: now,
        },
    };

    (Some(health_cond), update_cond)
}

async fn check_update_available(
    client: &servarr_api::ServarrClient,
    now: &str,
) -> Option<Condition> {
    let updates = match client.updates().await {
        Ok(u) => u,
        Err(_) => return None,
    };

    let available = updates.iter().find(|u| !u.installed && u.installable);
    Some(match available {
        Some(update) => Condition::ok(
            condition_types::UPDATE_AVAILABLE,
            "UpdateAvailable",
            &format!("Version {} is available", update.version),
            now,
        ),
        None => Condition::fail(
            condition_types::UPDATE_AVAILABLE,
            "UpToDate",
            "Running latest version",
            now,
        ),
    })
}

async fn update_status(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    name: &str,
    health_condition: Option<Condition>,
    update_condition: Option<Condition>,
    backup_status: Option<servarr_crds::BackupStatus>,
) -> Result<(), Error> {
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), ns);
    let (ready, ready_replicas) = match deploy_api.get(name).await {
        Ok(deploy) => {
            let replicas = deploy
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or(0);
            (replicas > 0, replicas)
        }
        Err(_) => (false, 0),
    };

    let generation = app.metadata.generation.unwrap_or(0);
    let now = chrono_now();
    let mut status = ServarrAppStatus {
        ready,
        ready_replicas,
        observed_generation: generation,
        conditions: Vec::new(),
        backup_status,
    };

    // DeploymentReady
    if ready {
        status.set_condition(Condition::ok(
            condition_types::DEPLOYMENT_READY,
            "ReplicasAvailable",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        ));
    } else {
        status.set_condition(Condition::fail(
            condition_types::DEPLOYMENT_READY,
            "ReplicasUnavailable",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        ));
    }

    // ServiceReady — we just applied it, so mark true
    status.set_condition(Condition::ok(
        condition_types::SERVICE_READY,
        "Applied",
        "Service applied",
        &now,
    ));

    // Progressing is false now (reconcile completed)
    status.set_condition(Condition::fail(
        condition_types::PROGRESSING,
        "ReconcileComplete",
        "Reconciliation finished",
        &now,
    ));

    // Overall Ready
    status.set_condition(if ready {
        Condition::ok(
            condition_types::READY,
            "DeploymentReady",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        )
    } else {
        Condition::fail(
            condition_types::READY,
            "DeploymentNotReady",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        )
    });

    // Degraded
    if !ready {
        status.set_condition(Condition::ok(
            condition_types::DEGRADED,
            "DeploymentNotReady",
            &format!("{ready_replicas} replica(s) ready"),
            &now,
        ));
    } else {
        status.set_condition(Condition::fail(
            condition_types::DEGRADED,
            "AllHealthy",
            "All resources healthy",
            &now,
        ));
    }

    // API health condition
    if let Some(cond) = health_condition {
        status.set_condition(cond);
    }
    // Update available condition
    if let Some(cond) = update_condition {
        status.set_condition(cond);
    }

    let status_patch = serde_json::json!({
        "apiVersion": "servarr.dev/v1alpha1",
        "kind": "ServarrApp",
        "status": status,
    });

    let apps = Api::<ServarrApp>::namespaced(client.clone(), ns);
    apps.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(status_patch),
    )
    .await
    .map_err(Error::Kube)?;

    Ok(())
}

fn error_policy(app: Arc<ServarrApp>, error: &Error, ctx: Arc<Context>) -> Action {
    let app_type = app.spec.app.as_str();
    increment_reconcile_total(app_type, "error");
    warn!(%error, "reconciliation failed, requeuing");

    let recorder = Recorder::new(ctx.client.clone(), ctx.reporter.clone());
    let obj_ref = app.object_ref(&());
    let error_msg = error.to_string();
    tokio::spawn(async move {
        let _ = recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: "ReconcileError".into(),
                    note: Some(error_msg),
                    action: "Reconcile".into(),
                    secondary: None,
                },
                &obj_ref,
            )
            .await;
    });

    Action::requeue(Duration::from_secs(60))
}

async fn maybe_run_backup(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Option<servarr_crds::BackupStatus> {
    let backup_spec = app.spec.backup.as_ref()?;
    if !backup_spec.enabled || backup_spec.schedule.is_empty() {
        return None;
    }

    let secret_name = app.spec.api_key_secret.as_deref()?;
    let api_key = match servarr_api::read_secret_key(client, ns, secret_name, "api-key").await {
        Ok(k) => k,
        Err(e) => {
            warn!(error = %e, "backup: failed to read API key");
            return Some(servarr_crds::BackupStatus {
                last_backup_result: Some(format!("secret read error: {e}")),
                ..Default::default()
            });
        }
    };

    // Only Servarr v3 apps support backup API
    if !matches!(
        app.spec.app,
        AppType::Sonarr | AppType::Radarr | AppType::Lidarr | AppType::Prowlarr
    ) {
        return None;
    }

    // Check if backup is due based on cron schedule
    let schedule = match cron::Schedule::from_str(&backup_spec.schedule) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, schedule = %backup_spec.schedule, "invalid cron schedule");
            return Some(servarr_crds::BackupStatus {
                last_backup_result: Some(format!("invalid schedule: {e}")),
                ..Default::default()
            });
        }
    };

    use chrono::Utc;
    let now = Utc::now();

    // Check last backup time from existing status
    let last_backup = app
        .status
        .as_ref()
        .and_then(|s| s.backup_status.as_ref())
        .and_then(|bs| bs.last_backup_time.as_deref())
        .and_then(|t| t.parse::<chrono::DateTime<Utc>>().ok());

    let is_due = match last_backup {
        Some(last) => schedule.after(&last).take(1).any(|next| next <= now),
        None => true, // Never backed up, do it now
    };

    if !is_due {
        // Return existing status unchanged
        return app.status.as_ref().and_then(|s| s.backup_status.clone());
    }

    let app_name = servarr_resources::common::app_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let base_url = format!("http://{app_name}.{ns}.svc:{port}");

    let api_client =
        match servarr_api::ServarrClient::new(&base_url, &api_key, app_type_to_kind(&app.spec.app))
        {
            Ok(c) => c,
            Err(e) => {
                return Some(servarr_crds::BackupStatus {
                    last_backup_result: Some(format!("client error: {e}")),
                    ..Default::default()
                });
            }
        };

    let app_type = app.spec.app.as_str();
    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "BackupStarted".into(),
                note: Some("Scheduled backup started".into()),
                action: "Backup".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await;

    info!(app = %app_name, "creating backup");
    match api_client.create_backup().await {
        Ok(backup) => {
            info!(app = %app_name, backup_id = backup.id, "backup created");
            increment_backup_operations(app_type, "backup", "success");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "BackupCompleted".into(),
                        note: Some(format!("Backup {} created successfully", backup.id)),
                        action: "Backup".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;

            // Prune old backups if over retention count
            let retention = backup_spec.retention_count;
            if let Ok(backups) = api_client.list_backups().await
                && backups.len() as u32 > retention
            {
                let mut sorted = backups;
                sorted.sort_by(|a, b| a.time.cmp(&b.time));
                let to_delete = sorted.len() - retention as usize;
                for old in sorted.iter().take(to_delete) {
                    if let Err(e) = api_client.delete_backup(old.id).await {
                        warn!(backup_id = old.id, error = %e, "failed to prune old backup");
                    }
                }
            }

            Some(servarr_crds::BackupStatus {
                last_backup_time: Some(chrono_now()),
                last_backup_result: Some("success".into()),
                backup_count: retention.min(
                    api_client
                        .list_backups()
                        .await
                        .map(|b| b.len() as u32)
                        .unwrap_or(0),
                ),
            })
        }
        Err(e) => {
            warn!(app = %app_name, error = %e, "backup failed");
            increment_backup_operations(app_type, "backup", "error");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "BackupFailed".into(),
                        note: Some(format!("Backup failed: {e}")),
                        action: "Backup".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;
            Some(servarr_crds::BackupStatus {
                last_backup_time: last_backup.map(|_| chrono_now()),
                last_backup_result: Some(format!("error: {e}")),
                backup_count: 0,
            })
        }
    }
}

/// Handle restore-from-backup triggered by the `servarr.dev/restore-from` annotation.
/// Scales the Deployment to 0, calls restore via the API, scales back up, and removes
/// the annotation to prevent re-triggering.
async fn maybe_restore_backup(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    name: &str,
    restore_id: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) {
    let backup_id: i64 = match restore_id.parse() {
        Ok(id) => id,
        Err(_) => {
            warn!(%name, restore_id, "invalid restore-from annotation value, expected integer backup ID");
            return;
        }
    };

    info!(%name, backup_id, "restore-from-backup triggered");

    let deploy_api = Api::<Deployment>::namespaced(client.clone(), ns);

    // Step 1: Scale deployment to 0
    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "RestoreStarted".into(),
                note: Some(format!("Scaling down for restore from backup {backup_id}")),
                action: "Restore".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await;

    let scale_down = serde_json::json!({
        "spec": { "replicas": 0 }
    });
    if let Err(e) = deploy_api
        .patch(name, &PatchParams::default(), &Patch::Merge(scale_down))
        .await
    {
        warn!(%name, error = %e, "failed to scale down for restore");
        return;
    }

    // Wait for pods to terminate (poll for up to 60 seconds)
    for _ in 0..12 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        match deploy_api.get(name).await {
            Ok(d) => {
                let ready = d
                    .status
                    .as_ref()
                    .and_then(|s| s.ready_replicas)
                    .unwrap_or(0);
                if ready == 0 {
                    break;
                }
            }
            Err(e) => {
                warn!(%name, error = %e, "failed to check deployment status during restore");
                break;
            }
        }
    }

    // Step 2: Build API client and call restore
    let api_key = match app.spec.api_key_secret.as_deref() {
        Some(secret_name) => {
            match servarr_api::read_secret_key(client, ns, secret_name, "api-key").await {
                Ok(k) => k,
                Err(e) => {
                    warn!(%name, error = %e, "failed to read API key for restore");
                    // Scale back up before returning
                    let scale_up = serde_json::json!({ "spec": { "replicas": 1 } });
                    let _ = deploy_api
                        .patch(name, &PatchParams::default(), &Patch::Merge(scale_up))
                        .await;
                    return;
                }
            }
        }
        None => {
            warn!(%name, "no api_key_secret configured, cannot restore");
            let scale_up = serde_json::json!({ "spec": { "replicas": 1 } });
            let _ = deploy_api
                .patch(name, &PatchParams::default(), &Patch::Merge(scale_up))
                .await;
            return;
        }
    };

    let app_name = servarr_resources::common::app_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let base_url = format!("http://{app_name}.{ns}.svc:{port}");

    // Only Servarr v3 apps (Sonarr/Radarr/Lidarr/Prowlarr) support backup/restore
    let restore_result =
        match servarr_api::ServarrClient::new(&base_url, &api_key, app_type_to_kind(&app.spec.app))
        {
            Ok(c) => c.restore_backup(backup_id).await,
            Err(e) => {
                warn!(%name, error = %e, "failed to create API client for restore");
                let scale_up = serde_json::json!({ "spec": { "replicas": 1 } });
                let _ = deploy_api
                    .patch(name, &PatchParams::default(), &Patch::Merge(scale_up))
                    .await;
                return;
            }
        };

    match restore_result {
        Ok(()) => {
            info!(%name, backup_id, "restore completed successfully");
            increment_backup_operations(app.spec.app.as_str(), "restore", "success");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "RestoreComplete".into(),
                        note: Some(format!("Successfully restored from backup {backup_id}")),
                        action: "Restore".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;
        }
        Err(e) => {
            warn!(%name, backup_id, error = %e, "restore API call failed");
            increment_backup_operations(app.spec.app.as_str(), "restore", "error");
            let _ = recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "RestoreFailed".into(),
                        note: Some(format!("Failed to restore from backup {backup_id}: {e}")),
                        action: "Restore".into(),
                        secondary: None,
                    },
                    obj_ref,
                )
                .await;
        }
    }

    // Step 3: Scale back up
    let scale_up = serde_json::json!({ "spec": { "replicas": 1 } });
    if let Err(e) = deploy_api
        .patch(name, &PatchParams::default(), &Patch::Merge(scale_up))
        .await
    {
        warn!(%name, error = %e, "failed to scale back up after restore");
    }

    // Step 4: Remove the restore-from annotation to prevent re-triggering
    let servarr_api_resource = Api::<ServarrApp>::namespaced(client.clone(), ns);
    let remove_annotation = serde_json::json!({
        "metadata": {
            "annotations": {
                "servarr.dev/restore-from": null
            }
        }
    });
    if let Err(e) = servarr_api_resource
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(remove_annotation),
        )
        .await
    {
        warn!(%name, error = %e, "failed to remove restore-from annotation");
    }
}

/// A discovered *arr app in the namespace with its service URL and API key.
struct DiscoveredApp {
    name: String,
    app_type: AppType,
    base_url: String,
    api_key: String,
    instance: Option<String>,
}

/// Discover all Servarr v3 apps (Sonarr/Radarr/Lidarr) in a namespace
/// and resolve their service URLs and API keys.
async fn discover_namespace_apps(
    client: &Client,
    namespace: &str,
) -> Result<Vec<DiscoveredApp>, anyhow::Error> {
    use kube::api::ListParams;

    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let apps = api
        .list(&ListParams::default())
        .await
        .map_err(|e| anyhow::anyhow!("failed to list ServarrApps: {e}"))?;

    let mut discovered = Vec::new();
    for app in &apps {
        // Only sync Servarr v3 apps (they share the /api/v3 interface)
        if !matches!(
            app.spec.app,
            AppType::Sonarr | AppType::Radarr | AppType::Lidarr
        ) {
            continue;
        }

        let secret_name = match app.spec.api_key_secret.as_deref() {
            Some(s) => s,
            None => continue,
        };

        let api_key = match servarr_api::read_secret_key(client, namespace, secret_name, "api-key")
            .await
        {
            Ok(k) => k,
            Err(e) => {
                warn!(app = %app.name_any(), error = %e, "skipping app: failed to read API key");
                continue;
            }
        };

        let app_name = servarr_resources::common::app_name(app);
        let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
        let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
        let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
        let base_url = format!("http://{app_name}.{namespace}.svc:{port}");

        discovered.push(DiscoveredApp {
            name: app.name_any(),
            app_type: app.spec.app.clone(),
            base_url,
            api_key,
            instance: app.spec.instance.clone(),
        });
    }

    Ok(discovered)
}

/// Sync discovered namespace apps into Prowlarr as registered applications.
async fn sync_prowlarr_apps(
    client: &Client,
    prowlarr: &ServarrApp,
    target_ns: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    let prowlarr_name = prowlarr.name_any();
    let ns = prowlarr.namespace().unwrap_or_else(|| "default".into());

    // Build Prowlarr client
    let secret_name = prowlarr
        .spec
        .api_key_secret
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Prowlarr sync requires api_key_secret"))?;
    let prowlarr_key = servarr_api::read_secret_key(client, &ns, secret_name, "api-key").await?;

    let prowlarr_app_name = servarr_resources::common::app_name(prowlarr);
    let defaults = servarr_crds::AppDefaults::for_app(&prowlarr.spec.app);
    let svc_spec = prowlarr.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let prowlarr_url = format!("http://{prowlarr_app_name}.{ns}.svc:{port}");

    let prowlarr_client = servarr_api::ProwlarrClient::new(&prowlarr_url, &prowlarr_key)?;

    // Discover apps in target namespace
    let discovered = discover_namespace_apps(client, target_ns).await?;

    // Get current Prowlarr applications
    let existing = prowlarr_client.list_applications().await?;

    // Build a map of existing apps by base URL for diffing
    let existing_by_url: std::collections::HashMap<String, &servarr_api::prowlarr::ProwlarrApp> =
        existing
            .iter()
            .filter_map(|a| {
                a.fields
                    .iter()
                    .find(|f| f.name == "baseUrl")
                    .and_then(|f| f.value.as_str())
                    .map(|url| (url.to_string(), a))
            })
            .collect();

    let auto_remove = prowlarr
        .spec
        .prowlarr_sync
        .as_ref()
        .map(|s| s.auto_remove)
        .unwrap_or(true);

    // Add or update discovered apps
    let mut synced_urls = std::collections::HashSet::new();
    for app in &discovered {
        synced_urls.insert(app.base_url.clone());

        let implementation = match app.app_type {
            AppType::Sonarr => "Sonarr",
            AppType::Radarr => "Radarr",
            AppType::Lidarr => "Lidarr",
            _ => continue,
        };

        let config_contract = match app.app_type {
            AppType::Sonarr => "SonarrSettings",
            AppType::Radarr => "RadarrSettings",
            AppType::Lidarr => "LidarrSettings",
            _ => continue,
        };

        let new_app = servarr_api::prowlarr::ProwlarrApp {
            id: 0,
            name: app.name.clone(),
            sync_level: "fullSync".into(),
            implementation: implementation.into(),
            config_contract: config_contract.into(),
            fields: vec![
                servarr_api::prowlarr::ProwlarrAppField {
                    name: "baseUrl".into(),
                    value: serde_json::Value::String(app.base_url.clone()),
                },
                servarr_api::prowlarr::ProwlarrAppField {
                    name: "apiKey".into(),
                    value: serde_json::Value::String(app.api_key.clone()),
                },
            ],
            tags: Vec::new(),
        };

        if let Some(existing_app) = existing_by_url.get(&app.base_url) {
            // Update if name changed
            if existing_app.name != app.name {
                info!(prowlarr = %prowlarr_name, app = %app.name, "updating Prowlarr application");
                let mut updated = new_app;
                updated.id = existing_app.id;
                if let Err(e) = prowlarr_client
                    .update_application(existing_app.id, &updated)
                    .await
                {
                    warn!(app = %app.name, error = %e, "failed to update Prowlarr application");
                }
            }
        } else {
            // Add new
            info!(prowlarr = %prowlarr_name, app = %app.name, "adding application to Prowlarr");
            if let Err(e) = prowlarr_client.add_application(&new_app).await {
                warn!(app = %app.name, error = %e, "failed to add Prowlarr application");
            }
        }
    }

    // Remove stale apps (those in Prowlarr but not discovered)
    if auto_remove {
        for app in &existing {
            let url = app
                .fields
                .iter()
                .find(|f| f.name == "baseUrl")
                .and_then(|f| f.value.as_str())
                .unwrap_or("");
            if !url.is_empty() && !synced_urls.contains(url) {
                info!(prowlarr = %prowlarr_name, app = %app.name, "removing stale application from Prowlarr");
                if let Err(e) = prowlarr_client.delete_application(app.id).await {
                    warn!(app = %app.name, error = %e, "failed to remove Prowlarr application");
                }
            }
        }
    }

    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "ProwlarrSyncComplete".into(),
                note: Some(format!("Synced {} apps to Prowlarr", discovered.len())),
                action: "ProwlarrSync".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await;

    Ok(())
}

/// Check if any Prowlarr instance with prowlarr_sync.enabled exists in the namespace.
async fn prowlarr_sync_exists(client: &Client, namespace: &str) -> bool {
    use kube::api::ListParams;
    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    match api.list(&ListParams::default()).await {
        Ok(list) => list.iter().any(|a| {
            a.spec.app == AppType::Prowlarr
                && a.spec.prowlarr_sync.as_ref().is_some_and(|s| s.enabled)
        }),
        Err(_) => false,
    }
}

/// Remove this app's registration from Prowlarr when the CR is deleted.
async fn cleanup_prowlarr_registration(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    use kube::api::ListParams;

    let app_name_str = servarr_resources::common::app_name(app);
    let defaults = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let app_url = format!("http://{app_name_str}.{namespace}.svc:{port}");

    // Find the Prowlarr instance
    let sa_api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let apps = sa_api.list(&ListParams::default()).await?;
    let prowlarr = apps.iter().find(|a| {
        a.spec.app == AppType::Prowlarr && a.spec.prowlarr_sync.as_ref().is_some_and(|s| s.enabled)
    });

    let prowlarr = match prowlarr {
        Some(p) => p,
        None => return Ok(()), // No Prowlarr with sync, nothing to clean up
    };

    let secret_name = match prowlarr.spec.api_key_secret.as_deref() {
        Some(s) => s,
        None => return Ok(()),
    };

    let prowlarr_key =
        servarr_api::read_secret_key(client, namespace, secret_name, "api-key").await?;

    let prowlarr_app_name = servarr_resources::common::app_name(prowlarr);
    let prowlarr_defaults = servarr_crds::AppDefaults::for_app(&prowlarr.spec.app);
    let prowlarr_svc = prowlarr
        .spec
        .service
        .as_ref()
        .unwrap_or(&prowlarr_defaults.service);
    let prowlarr_port = prowlarr_svc.ports.first().map(|p| p.port).unwrap_or(80);
    let prowlarr_ns = prowlarr.namespace().unwrap_or_else(|| namespace.into());
    let prowlarr_url = format!("http://{prowlarr_app_name}.{prowlarr_ns}.svc:{prowlarr_port}");

    let prowlarr_client = servarr_api::ProwlarrClient::new(&prowlarr_url, &prowlarr_key)?;

    let existing = prowlarr_client.list_applications().await?;
    if let Some(registered) = existing.iter().find(|a| {
        a.fields
            .iter()
            .any(|f| f.name == "baseUrl" && f.value.as_str() == Some(&app_url))
    }) {
        info!(
            app = %app.name_any(),
            prowlarr_app_id = registered.id,
            "removing app from Prowlarr on deletion"
        );
        prowlarr_client.delete_application(registered.id).await?;

        let _ = recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "ProwlarrCleanup".into(),
                    note: Some(format!("Removed {} from Prowlarr", app.name_any())),
                    action: "Finalize".into(),
                    secondary: None,
                },
                obj_ref,
            )
            .await;
    }

    Ok(())
}

/// Sync discovered Sonarr/Radarr apps into Overseerr as registered servers.
async fn sync_overseerr_servers(
    client: &Client,
    overseerr: &ServarrApp,
    target_ns: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    let overseerr_name = overseerr.name_any();
    let ns = overseerr.namespace().unwrap_or_else(|| "default".into());

    // Build Overseerr client
    let secret_name = overseerr
        .spec
        .api_key_secret
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Overseerr sync requires api_key_secret"))?;
    let overseerr_key = servarr_api::read_secret_key(client, &ns, secret_name, "api-key").await?;

    let overseerr_app_name = servarr_resources::common::app_name(overseerr);
    let defaults = servarr_crds::AppDefaults::for_app(&overseerr.spec.app);
    let svc_spec = overseerr.spec.service.as_ref().unwrap_or(&defaults.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let overseerr_url = format!("http://{overseerr_app_name}.{ns}.svc:{port}");

    let overseerr_client = servarr_api::OverseerrClient::new(&overseerr_url, &overseerr_key);

    // Discover Sonarr/Radarr apps in target namespace
    let discovered = discover_namespace_apps(client, target_ns).await?;

    // Get existing server registrations
    let existing_sonarr = overseerr_client.list_sonarr().await?;
    let existing_radarr = overseerr_client.list_radarr().await?;

    // Get Overseerr config for default profile/directory settings
    let overseerr_config = match &overseerr.spec.app_config {
        Some(servarr_crds::AppConfig::Overseerr(c)) => Some(c.as_ref()),
        _ => None,
    };

    let auto_remove = overseerr
        .spec
        .overseerr_sync
        .as_ref()
        .map(|s| s.auto_remove)
        .unwrap_or(true);

    // Track which hostname:port combos we sync so we can detect stale entries
    let mut synced_sonarr_keys = std::collections::HashSet::new();
    let mut synced_radarr_keys = std::collections::HashSet::new();

    for app in &discovered {
        let url = url::Url::parse(&app.base_url).ok();
        let hostname = url
            .as_ref()
            .map(|u| u.host_str().unwrap_or("").to_string())
            .unwrap_or_default();
        let port = url.as_ref().and_then(|u| u.port()).unwrap_or(80) as f64;
        let is4k = app.instance.as_deref() == Some("4k");

        match app.app_type {
            AppType::Sonarr => {
                let key = (hostname.clone(), port as u16);
                synced_sonarr_keys.insert(key);

                let sonarr_defaults = overseerr_config.and_then(|c| c.sonarr.as_ref());
                let (profile_id, profile_name, root_folder, enable_season_folders) = if is4k {
                    let four_k = sonarr_defaults.and_then(|d| d.four_k.as_ref());
                    (
                        four_k.map(|f| f.profile_id).unwrap_or(0.0),
                        four_k.map(|f| f.profile_name.clone()).unwrap_or_default(),
                        four_k.map(|f| f.root_folder.clone()).unwrap_or_default(),
                        four_k.and_then(|f| f.enable_season_folders).unwrap_or(true),
                    )
                } else {
                    (
                        sonarr_defaults.map(|d| d.profile_id).unwrap_or(0.0),
                        sonarr_defaults
                            .map(|d| d.profile_name.clone())
                            .unwrap_or_default(),
                        sonarr_defaults
                            .map(|d| d.root_folder.clone())
                            .unwrap_or_default(),
                        sonarr_defaults
                            .and_then(|d| d.enable_season_folders)
                            .unwrap_or(true),
                    )
                };

                let settings = overseerr::models::SonarrSettings::new(
                    app.name.clone(),
                    hostname.clone(),
                    port,
                    app.api_key.clone(),
                    false,
                    profile_id,
                    profile_name,
                    root_folder,
                    is4k,
                    enable_season_folders,
                    !is4k,
                );

                // Match existing by hostname + port
                if let Some(existing) = existing_sonarr
                    .iter()
                    .find(|s| s.hostname == hostname && s.port == port)
                {
                    let id = existing.id.unwrap_or(0.0) as i32;
                    let mut updated = settings;
                    updated.id = existing.id;
                    if let Err(e) = overseerr_client.update_sonarr(id, updated).await {
                        warn!(app = %app.name, error = %e, "failed to update Sonarr in Overseerr");
                    }
                } else {
                    info!(overseerr = %overseerr_name, app = %app.name, "adding Sonarr server to Overseerr");
                    if let Err(e) = overseerr_client.create_sonarr(settings).await {
                        warn!(app = %app.name, error = %e, "failed to add Sonarr to Overseerr");
                    }
                }
            }
            AppType::Radarr => {
                let key = (hostname.clone(), port as u16);
                synced_radarr_keys.insert(key);

                let radarr_defaults = overseerr_config.and_then(|c| c.radarr.as_ref());
                let (profile_id, profile_name, root_folder, minimum_availability) = if is4k {
                    let four_k = radarr_defaults.and_then(|d| d.four_k.as_ref());
                    (
                        four_k.map(|f| f.profile_id).unwrap_or(0.0),
                        four_k.map(|f| f.profile_name.clone()).unwrap_or_default(),
                        four_k.map(|f| f.root_folder.clone()).unwrap_or_default(),
                        four_k
                            .and_then(|f| f.minimum_availability.clone())
                            .unwrap_or_else(|| "released".to_string()),
                    )
                } else {
                    (
                        radarr_defaults.map(|d| d.profile_id).unwrap_or(0.0),
                        radarr_defaults
                            .map(|d| d.profile_name.clone())
                            .unwrap_or_default(),
                        radarr_defaults
                            .map(|d| d.root_folder.clone())
                            .unwrap_or_default(),
                        radarr_defaults
                            .and_then(|d| d.minimum_availability.clone())
                            .unwrap_or_else(|| "released".to_string()),
                    )
                };

                let settings = overseerr::models::RadarrSettings::new(
                    app.name.clone(),
                    hostname.clone(),
                    port,
                    app.api_key.clone(),
                    false,
                    profile_id,
                    profile_name,
                    root_folder,
                    is4k,
                    minimum_availability,
                    !is4k,
                );

                // Match existing by hostname + port
                if let Some(existing) = existing_radarr
                    .iter()
                    .find(|s| s.hostname == hostname && s.port == port)
                {
                    let id = existing.id.unwrap_or(0.0) as i32;
                    let mut updated = settings;
                    updated.id = existing.id;
                    if let Err(e) = overseerr_client.update_radarr(id, updated).await {
                        warn!(app = %app.name, error = %e, "failed to update Radarr in Overseerr");
                    }
                } else {
                    info!(overseerr = %overseerr_name, app = %app.name, "adding Radarr server to Overseerr");
                    if let Err(e) = overseerr_client.create_radarr(settings).await {
                        warn!(app = %app.name, error = %e, "failed to add Radarr to Overseerr");
                    }
                }
            }
            _ => continue,
        }
    }

    // Remove stale servers
    if auto_remove {
        for existing in &existing_sonarr {
            let key = (existing.hostname.clone(), existing.port as u16);
            if !synced_sonarr_keys.contains(&key) {
                let id = existing.id.unwrap_or(0.0) as i32;
                info!(overseerr = %overseerr_name, server = %existing.name, "removing stale Sonarr server from Overseerr");
                if let Err(e) = overseerr_client.delete_sonarr(id).await {
                    warn!(server = %existing.name, error = %e, "failed to remove stale Sonarr from Overseerr");
                }
            }
        }
        for existing in &existing_radarr {
            let key = (existing.hostname.clone(), existing.port as u16);
            if !synced_radarr_keys.contains(&key) {
                let id = existing.id.unwrap_or(0.0) as i32;
                info!(overseerr = %overseerr_name, server = %existing.name, "removing stale Radarr server from Overseerr");
                if let Err(e) = overseerr_client.delete_radarr(id).await {
                    warn!(server = %existing.name, error = %e, "failed to remove stale Radarr from Overseerr");
                }
            }
        }
    }

    let sonarr_count = discovered
        .iter()
        .filter(|a| a.app_type == AppType::Sonarr)
        .count();
    let radarr_count = discovered
        .iter()
        .filter(|a| a.app_type == AppType::Radarr)
        .count();
    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "OverseerrSyncComplete".into(),
                note: Some(format!(
                    "Synced {sonarr_count} Sonarr + {radarr_count} Radarr servers to Overseerr"
                )),
                action: "OverseerrSync".into(),
                secondary: None,
            },
            obj_ref,
        )
        .await;

    Ok(())
}

/// Check if any Overseerr instance with overseerr_sync.enabled exists in the namespace.
async fn overseerr_sync_exists(client: &Client, namespace: &str) -> bool {
    use kube::api::ListParams;
    let api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    match api.list(&ListParams::default()).await {
        Ok(list) => list.iter().any(|a| {
            a.spec.app == AppType::Overseerr
                && a.spec.overseerr_sync.as_ref().is_some_and(|s| s.enabled)
        }),
        Err(_) => false,
    }
}

/// Remove this app's registration from Overseerr when the CR is deleted.
async fn cleanup_overseerr_registration(
    client: &Client,
    app: &ServarrApp,
    namespace: &str,
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) -> Result<(), anyhow::Error> {
    use kube::api::ListParams;

    let app_name_str = servarr_resources::common::app_name(app);
    let defaults_for_app = servarr_crds::AppDefaults::for_app(&app.spec.app);
    let svc_spec = app
        .spec
        .service
        .as_ref()
        .unwrap_or(&defaults_for_app.service);
    let port = svc_spec.ports.first().map(|p| p.port).unwrap_or(80);
    let app_hostname = format!("{app_name_str}.{namespace}.svc");

    // Find the Overseerr instance
    let sa_api = Api::<ServarrApp>::namespaced(client.clone(), namespace);
    let apps = sa_api.list(&ListParams::default()).await?;
    let overseerr = apps.iter().find(|a| {
        a.spec.app == AppType::Overseerr
            && a.spec.overseerr_sync.as_ref().is_some_and(|s| s.enabled)
    });

    let overseerr = match overseerr {
        Some(o) => o,
        None => return Ok(()),
    };

    let secret_name = match overseerr.spec.api_key_secret.as_deref() {
        Some(s) => s,
        None => return Ok(()),
    };

    let overseerr_ns = overseerr.namespace().unwrap_or_else(|| namespace.into());
    let overseerr_key =
        servarr_api::read_secret_key(client, &overseerr_ns, secret_name, "api-key").await?;

    let overseerr_app_name = servarr_resources::common::app_name(overseerr);
    let overseerr_defaults = servarr_crds::AppDefaults::for_app(&overseerr.spec.app);
    let overseerr_svc = overseerr
        .spec
        .service
        .as_ref()
        .unwrap_or(&overseerr_defaults.service);
    let overseerr_port = overseerr_svc.ports.first().map(|p| p.port).unwrap_or(80);
    let overseerr_url = format!("http://{overseerr_app_name}.{overseerr_ns}.svc:{overseerr_port}");

    let overseerr_client = servarr_api::OverseerrClient::new(&overseerr_url, &overseerr_key);

    // Remove matching Sonarr or Radarr server by hostname + port
    match app.spec.app {
        AppType::Sonarr => {
            let existing = overseerr_client.list_sonarr().await?;
            if let Some(registered) = existing
                .iter()
                .find(|s| s.hostname == app_hostname && s.port == port as f64)
            {
                let id = registered.id.unwrap_or(0.0) as i32;
                info!(
                    app = %app.name_any(),
                    overseerr_server_id = id,
                    "removing Sonarr from Overseerr on deletion"
                );
                overseerr_client.delete_sonarr(id).await?;

                let _ = recorder
                    .publish(
                        &Event {
                            type_: EventType::Normal,
                            reason: "OverseerrCleanup".into(),
                            note: Some(format!("Removed {} from Overseerr", app.name_any())),
                            action: "Finalize".into(),
                            secondary: None,
                        },
                        obj_ref,
                    )
                    .await;
            }
        }
        AppType::Radarr => {
            let existing = overseerr_client.list_radarr().await?;
            if let Some(registered) = existing
                .iter()
                .find(|s| s.hostname == app_hostname && s.port == port as f64)
            {
                let id = registered.id.unwrap_or(0.0) as i32;
                info!(
                    app = %app.name_any(),
                    overseerr_server_id = id,
                    "removing Radarr from Overseerr on deletion"
                );
                overseerr_client.delete_radarr(id).await?;

                let _ = recorder
                    .publish(
                        &Event {
                            type_: EventType::Normal,
                            reason: "OverseerrCleanup".into(),
                            note: Some(format!("Removed {} from Overseerr", app.name_any())),
                            action: "Finalize".into(),
                            secondary: None,
                        },
                        obj_ref,
                    )
                    .await;
            }
        }
        _ => {}
    }

    Ok(())
}

fn chrono_now() -> String {
    // ISO 8601 timestamp with seconds precision
    use chrono::{SecondsFormat, Utc};
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Return paths where `desired` differs from `actual` for debugging drift.
fn json_diff_paths(
    desired: &serde_json::Value,
    actual: &serde_json::Value,
    path: String,
) -> Vec<String> {
    use serde_json::Value;
    match (desired, actual) {
        (Value::Object(d), Value::Object(a)) => d
            .iter()
            .flat_map(|(k, dv)| {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match a.get(k) {
                    Some(av) => json_diff_paths(dv, av, p),
                    None => vec![format!("{p}: missing in actual")],
                }
            })
            .collect(),
        (Value::Array(d), Value::Array(a)) if d.len() == a.len() => d
            .iter()
            .zip(a.iter())
            .enumerate()
            .flat_map(|(i, (dv, av))| json_diff_paths(dv, av, format!("{path}[{i}]")))
            .collect(),
        (Value::Array(d), Value::Array(a)) => {
            vec![format!("{path}: array length {0} vs {1}", d.len(), a.len())]
        }
        _ if desired == actual => vec![],
        _ => vec![format!("{path}: {desired} vs {actual}")],
    }
}

/// Check that every field in `desired` exists with the same value in `actual`.
/// Extra fields in `actual` (e.g. Kubernetes defaults) are ignored.
fn json_is_subset(desired: &serde_json::Value, actual: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (desired, actual) {
        (Value::Object(d), Value::Object(a)) => d
            .iter()
            .all(|(k, dv)| a.get(k).is_some_and(|av| json_is_subset(dv, av))),
        (Value::Array(d), Value::Array(a)) => {
            d.len() == a.len()
                && d.iter()
                    .zip(a.iter())
                    .all(|(dv, av)| json_is_subset(dv, av))
        }
        // Leaf values: exact match
        _ => desired == actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- json_is_subset ----

    #[test]
    fn json_is_subset_both_empty_objects() {
        assert!(json_is_subset(&json!({}), &json!({})));
    }

    #[test]
    fn json_is_subset_extra_keys_in_actual() {
        assert!(json_is_subset(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
    }

    #[test]
    fn json_is_subset_value_mismatch() {
        assert!(!json_is_subset(&json!({"a": 1}), &json!({"a": 2})));
    }

    #[test]
    fn json_is_subset_missing_key_in_actual() {
        assert!(!json_is_subset(&json!({"a": 1}), &json!({})));
    }

    #[test]
    fn json_is_subset_nested_objects_extra_keys() {
        assert!(json_is_subset(
            &json!({"a": {"b": 1}}),
            &json!({"a": {"b": 1, "c": 2}})
        ));
    }

    #[test]
    fn json_is_subset_arrays_same() {
        assert!(json_is_subset(&json!([1, 2, 3]), &json!([1, 2, 3])));
    }

    #[test]
    fn json_is_subset_arrays_different_lengths() {
        assert!(!json_is_subset(&json!([1, 2]), &json!([1, 2, 3])));
    }

    #[test]
    fn json_is_subset_arrays_different_values() {
        assert!(!json_is_subset(&json!([1, 2, 3]), &json!([1, 2, 4])));
    }

    #[test]
    fn json_is_subset_null_vs_null() {
        assert!(json_is_subset(&json!(null), &json!(null)));
    }

    #[test]
    fn json_is_subset_string_equality() {
        assert!(json_is_subset(&json!("hello"), &json!("hello")));
    }

    #[test]
    fn json_is_subset_string_inequality() {
        assert!(!json_is_subset(&json!("hello"), &json!("world")));
    }

    #[test]
    fn json_is_subset_number_equality() {
        assert!(json_is_subset(&json!(42), &json!(42)));
    }

    #[test]
    fn json_is_subset_mixed_types() {
        assert!(!json_is_subset(&json!(1), &json!("1")));
    }

    #[test]
    fn json_is_subset_deeply_nested_match() {
        let desired = json!({"a": {"b": {"c": {"d": 1}}}});
        let actual = json!({"a": {"b": {"c": {"d": 1, "e": 2}, "f": 3}}, "g": 4});
        assert!(json_is_subset(&desired, &actual));
    }

    #[test]
    fn json_is_subset_deeply_nested_mismatch() {
        let desired = json!({"a": {"b": {"c": {"d": 1}}}});
        let actual = json!({"a": {"b": {"c": {"d": 99}}}});
        assert!(!json_is_subset(&desired, &actual));
    }

    // ---- json_diff_paths ----

    #[test]
    fn json_diff_paths_both_empty_objects() {
        let result = json_diff_paths(&json!({}), &json!({}), String::new());
        assert!(result.is_empty());
    }

    #[test]
    fn json_diff_paths_missing_key() {
        let result = json_diff_paths(&json!({"key": 1}), &json!({}), String::new());
        assert_eq!(result, vec!["key: missing in actual"]);
    }

    #[test]
    fn json_diff_paths_different_value() {
        let result = json_diff_paths(&json!({"key": 1}), &json!({"key": 2}), String::new());
        assert_eq!(result, vec!["key: 1 vs 2"]);
    }

    #[test]
    fn json_diff_paths_nested_difference() {
        let result = json_diff_paths(
            &json!({"parent": {"child": 1}}),
            &json!({"parent": {"child": 2}}),
            String::new(),
        );
        assert_eq!(result, vec!["parent.child: 1 vs 2"]);
    }

    #[test]
    fn json_diff_paths_array_length_mismatch() {
        let result = json_diff_paths(&json!({"a": [1, 2]}), &json!({"a": [1]}), String::new());
        assert_eq!(result, vec!["a: array length 2 vs 1"]);
    }

    #[test]
    fn json_diff_paths_array_element_difference() {
        let result =
            json_diff_paths(&json!({"a": [1, 2]}), &json!({"a": [1, 3]}), String::new());
        assert_eq!(result, vec!["a[1]: 2 vs 3"]);
    }

    #[test]
    fn json_diff_paths_multiple_differences() {
        let result = json_diff_paths(
            &json!({"a": 1, "b": 2}),
            &json!({"a": 10, "b": 20}),
            String::new(),
        );
        assert_eq!(result.len(), 2);
        assert!(result.contains(&"a: 1 vs 10".to_string()));
        assert!(result.contains(&"b: 2 vs 20".to_string()));
    }

    #[test]
    fn json_diff_paths_root_path_empty_no_leading_dot() {
        let result = json_diff_paths(&json!({"x": 1}), &json!({"x": 2}), String::new());
        // Should be "x: ..." not ".x: ..."
        assert!(result[0].starts_with("x:"));
    }

    // ---- app_type_to_kind ----

    #[test]
    fn app_type_to_kind_sonarr() {
        assert!(matches!(app_type_to_kind(&AppType::Sonarr), AppKind::Sonarr));
    }

    #[test]
    fn app_type_to_kind_radarr() {
        assert!(matches!(app_type_to_kind(&AppType::Radarr), AppKind::Radarr));
    }

    #[test]
    fn app_type_to_kind_lidarr() {
        assert!(matches!(app_type_to_kind(&AppType::Lidarr), AppKind::Lidarr));
    }

    #[test]
    fn app_type_to_kind_prowlarr() {
        assert!(matches!(
            app_type_to_kind(&AppType::Prowlarr),
            AppKind::Prowlarr
        ));
    }

    #[test]
    #[should_panic(expected = "AppKind not supported")]
    fn app_type_to_kind_unsupported_panics() {
        app_type_to_kind(&AppType::Sabnzbd);
    }

    // ---- chrono_now ----

    #[test]
    fn chrono_now_returns_valid_iso8601() {
        let now = chrono_now();
        assert!(now.contains('T'), "should contain T separator: {now}");
        assert!(now.ends_with('Z'), "should end with Z: {now}");
    }

    // ---- print_crd ----

    #[test]
    fn print_crd_returns_ok() {
        assert!(print_crd().is_ok());
    }
}
