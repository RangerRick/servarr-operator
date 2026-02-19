use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, CustomResourceExt, Resource, ResourceExt};
use servarr_crds::{
    AppType, Condition, MediaStack, MediaStackStatus, ServarrApp, ServarrAppSpec, StackAppStatus,
    StackPhase,
};
use thiserror::Error;
use tokio::time::Duration;
use tracing::{error, info, warn};

use crate::context::Context;
use crate::metrics::{
    increment_stack_reconcile_total, observe_stack_reconcile_duration, set_managed_stacks,
};

const FIELD_MANAGER: &str = "servarr-operator-stack";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Kubernetes API error: {0}")]
    Kube(#[source] kube::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
}

pub fn print_crd() -> Result<()> {
    let crd = MediaStack::crd();
    let yaml = serde_yaml::to_string(&crd)?;
    println!("{yaml}");
    Ok(())
}

pub async fn run(server_state: crate::server::ServerState) -> Result<()> {
    let client = Client::try_default().await?;
    let ctx = Arc::new(Context::new(client.clone()));

    let (stacks, apps) = if let Some(ref ns) = ctx.watch_namespace {
        (
            Api::<MediaStack>::namespaced(client.clone(), ns),
            Api::<ServarrApp>::namespaced(client.clone(), ns),
        )
    } else {
        (
            Api::<MediaStack>::all(client.clone()),
            Api::<ServarrApp>::all(client.clone()),
        )
    };

    info!("Starting media-stack controller");
    server_state.set_ready();

    Controller::new(stacks, watcher::Config::default())
        .owns(apps, watcher::Config::default())
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "media-stack reconciled"),
                Err(e) => error!(%e, "media-stack reconcile error"),
            }
        })
        .await;

    Ok(())
}

async fn reconcile(stack: Arc<MediaStack>, ctx: Arc<Context>) -> Result<Action, Error> {
    let client = &ctx.client;
    let name = stack.name_any();
    let ns = stack.namespace().unwrap_or_else(|| "default".into());
    let pp = PatchParams::apply(FIELD_MANAGER).force();

    info!(%name, %ns, "reconciling MediaStack");
    let start_time = std::time::Instant::now();

    let defaults = stack.spec.defaults.as_ref();

    // Collect enabled apps and expand split4k entries
    let mut expanded: Vec<(String, ServarrAppSpec, AppType, u8)> = Vec::new();
    for app in stack.spec.apps.iter().filter(|a| a.enabled) {
        match app.expand(&name, defaults) {
            Ok(pairs) => {
                for (child_name, spec) in pairs {
                    let tier = app.app.tier();
                    let app_type = spec.app.clone();
                    expanded.push((child_name, spec, app_type, tier));
                }
            }
            Err(msg) => {
                warn!(%name, error = %msg, "split4k validation failed");
                let now = chrono_now();
                let mut status = MediaStackStatus::default();
                status.set_condition(Condition::fail("Valid", "InvalidSplit4k", &msg, &now));
                status.observed_generation = stack.metadata.generation.unwrap_or(0);
                patch_status(client, &ns, &name, &status).await?;
                increment_stack_reconcile_total("error");
                return Ok(Action::requeue(Duration::from_secs(60)));
            }
        }
    }

    // Check for duplicate child names
    {
        let mut seen = HashSet::new();
        for (child_name, _, _, _) in &expanded {
            if !seen.insert(child_name.clone()) {
                warn!(%name, child = %child_name, "duplicate app+instance in MediaStack");
                let now = chrono_now();
                let mut status = MediaStackStatus::default();
                status.set_condition(Condition::fail(
                    "Valid",
                    "DuplicateApp",
                    &format!("Duplicate app+instance: {child_name}"),
                    &now,
                ));
                status.observed_generation = stack.metadata.generation.unwrap_or(0);
                patch_status(client, &ns, &name, &status).await?;
                increment_stack_reconcile_total("error");
                return Ok(Action::requeue(Duration::from_secs(60)));
            }
        }
    }

    // Group by tier
    let mut tiers: BTreeMap<u8, Vec<(String, ServarrAppSpec, AppType)>> = BTreeMap::new();
    for (child_name, spec, app_type, tier) in expanded {
        tiers
            .entry(tier)
            .or_default()
            .push((child_name, spec, app_type));
    }

    // Desired child names for orphan cleanup
    let desired_children: HashSet<String> = tiers
        .values()
        .flat_map(|apps| apps.iter().map(|(n, _, _)| n.clone()))
        .collect();

    let sa_api = Api::<ServarrApp>::namespaced(client.clone(), &ns);
    let mut app_statuses: Vec<StackAppStatus> = Vec::new();
    let mut ready_count: i32 = 0;
    let mut current_tier: Option<u8> = None;
    let mut all_previous_ready = true;

    // Iterate tiers in order
    for (&tier, apps) in &tiers {
        if tier > 0 && !all_previous_ready {
            // Previous tier not ready — record not-ready statuses and skip
            for (child_name, _, app_type) in apps {
                app_statuses.push(StackAppStatus {
                    name: child_name.clone(),
                    app_type: app_type.as_str().to_string(),
                    tier,
                    ready: false,
                    enabled: true,
                });
            }
            continue;
        }

        current_tier = Some(tier);

        for (child_name, spec, app_type) in apps {
            // Build child ServarrApp with ownerReferences and labels
            let owner_ref = stack
                .controller_owner_ref(&())
                .expect("stack should have UID");

            let child = ServarrApp::new(child_name, spec.clone());
            let mut child_value = serde_json::to_value(&child).map_err(Error::Serialization)?;

            // Inject metadata
            let meta = child_value
                .as_object_mut()
                .unwrap()
                .entry("metadata")
                .or_insert_with(|| serde_json::json!({}));
            let meta_obj = meta.as_object_mut().unwrap();
            meta_obj.insert("namespace".to_string(), serde_json::json!(ns));
            meta_obj.insert(
                "ownerReferences".to_string(),
                serde_json::to_value(vec![&owner_ref]).map_err(Error::Serialization)?,
            );
            meta_obj.insert(
                "labels".to_string(),
                serde_json::json!({
                    "servarr.dev/stack": name,
                    "servarr.dev/tier": tier.to_string(),
                    "app.kubernetes.io/managed-by": FIELD_MANAGER
                }),
            );

            sa_api
                .patch(child_name, &pp, &Patch::Apply(child_value))
                .await
                .map_err(Error::Kube)?;

            // Read back child status
            let is_ready = match sa_api.get(child_name).await {
                Ok(sa) => sa.status.as_ref().is_some_and(|s| s.ready),
                Err(_) => false,
            };

            if is_ready {
                ready_count += 1;
            } else {
                all_previous_ready = false;
            }

            app_statuses.push(StackAppStatus {
                name: child_name.clone(),
                app_type: app_type.as_str().to_string(),
                tier,
                ready: is_ready,
                enabled: true,
            });
        }
    }

    // Add disabled apps to statuses
    for app in &stack.spec.apps {
        if !app.enabled {
            app_statuses.push(StackAppStatus {
                name: app.child_name(&name),
                app_type: app.app.as_str().to_string(),
                tier: app.app.tier(),
                ready: false,
                enabled: false,
            });
        }
    }

    // Cleanup orphaned children
    let label_selector = format!("servarr.dev/stack={name}");
    let existing = sa_api
        .list(&ListParams::default().labels(&label_selector))
        .await
        .map_err(Error::Kube)?;

    for child in &existing {
        let child_name = child.name_any();
        if !desired_children.contains(&child_name) {
            info!(%name, child = %child_name, "deleting orphaned child ServarrApp");
            if let Err(e) = sa_api.delete(&child_name, &Default::default()).await {
                warn!(%name, child = %child_name, error = %e, "failed to delete orphaned child");
            }
        }
    }

    // Compute phase (total_apps is the expanded count)
    let total_apps = desired_children.len() as i32;
    let was_ready = stack
        .status
        .as_ref()
        .is_some_and(|s| s.phase == StackPhase::Ready);

    let phase = if total_apps == 0 {
        StackPhase::Pending
    } else if ready_count == total_apps {
        StackPhase::Ready
    } else if was_ready && ready_count < total_apps {
        StackPhase::Degraded
    } else if ready_count > 0 {
        StackPhase::RollingOut
    } else {
        StackPhase::Pending
    };

    let now = chrono_now();
    let mut status = MediaStackStatus {
        ready: phase == StackPhase::Ready,
        phase: phase.clone(),
        current_tier,
        total_apps,
        ready_apps: ready_count,
        app_statuses,
        conditions: Vec::new(),
        observed_generation: stack.metadata.generation.unwrap_or(0),
    };

    status.set_condition(Condition::ok("Valid", "Valid", "Spec is valid", &now));

    match &phase {
        StackPhase::Ready => {
            status.set_condition(Condition::ok(
                "Ready",
                "AllAppsReady",
                &format!("{ready_count}/{total_apps} apps ready"),
                &now,
            ));
        }
        StackPhase::RollingOut => {
            status.set_condition(Condition::fail(
                "Ready",
                "RollingOut",
                &format!(
                    "{ready_count}/{total_apps} apps ready, rolling out tier {}",
                    current_tier.unwrap_or(0)
                ),
                &now,
            ));
        }
        StackPhase::Degraded => {
            status.set_condition(Condition::fail(
                "Ready",
                "Degraded",
                &format!("{ready_count}/{total_apps} apps ready (was fully ready)"),
                &now,
            ));
        }
        StackPhase::Pending => {
            status.set_condition(Condition::fail(
                "Ready",
                "Pending",
                "No apps ready yet",
                &now,
            ));
        }
    }

    patch_status(client, &ns, &name, &status).await?;

    // Update managed-stacks gauge
    let gauge_api = if let Some(ref ns) = ctx.watch_namespace {
        Api::<MediaStack>::namespaced(client.clone(), ns)
    } else {
        Api::<MediaStack>::all(client.clone())
    };
    if let Ok(stack_list) = gauge_api.list(&ListParams::default()).await {
        let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for s in &stack_list.items {
            let key = s.namespace().unwrap_or_default();
            *counts.entry(key).or_default() += 1;
        }
        for (ns_key, count) in &counts {
            set_managed_stacks(ns_key, *count);
        }
    }

    let duration = start_time.elapsed().as_secs_f64();
    observe_stack_reconcile_duration(duration);
    increment_stack_reconcile_total("success");

    info!(%name, %phase, ready = ready_count, total = total_apps, "MediaStack reconciliation complete");

    // Requeue interval based on phase
    let requeue = match phase {
        StackPhase::Ready => Duration::from_secs(300),
        _ => Duration::from_secs(30),
    };

    Ok(Action::requeue(requeue))
}

async fn patch_status(
    client: &Client,
    ns: &str,
    name: &str,
    status: &MediaStackStatus,
) -> Result<(), Error> {
    let stacks = Api::<MediaStack>::namespaced(client.clone(), ns);
    let status_patch = serde_json::json!({
        "apiVersion": "servarr.dev/v1alpha1",
        "kind": "MediaStack",
        "status": status,
    });
    stacks
        .patch_status(
            name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(status_patch),
        )
        .await
        .map_err(Error::Kube)?;
    Ok(())
}

fn error_policy(_stack: Arc<MediaStack>, error: &Error, _ctx: Arc<Context>) -> Action {
    increment_stack_reconcile_total("error");
    warn!(%error, "media-stack reconciliation failed, requeuing");
    Action::requeue(Duration::from_secs(60))
}

fn chrono_now() -> String {
    use chrono::{SecondsFormat, Utc};
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
