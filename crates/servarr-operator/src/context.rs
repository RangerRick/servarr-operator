use kube::Client;
use kube::runtime::events::Reporter;
use servarr_crds::ImageSpec;
use std::collections::HashMap;
use tracing::info;

pub struct Context {
    pub client: Client,
    /// Image overrides loaded from DEFAULT_IMAGE_<APP>_REPO / DEFAULT_IMAGE_<APP>_TAG env vars.
    /// Keys are lowercase app names (e.g. "sonarr", "radarr").
    pub image_overrides: HashMap<String, ImageSpec>,
    /// Reporter identity used when publishing Kubernetes Events.
    pub reporter: Reporter,
    /// The namespace to watch. When `Some`, the operator uses `Api::namespaced()`
    /// and only needs `Role`/`RoleBinding` privileges. When `None`, the operator
    /// watches all namespaces and requires `ClusterRole`/`ClusterRoleBinding`.
    ///
    /// Defaults to the pod's own namespace (from `WATCH_NAMESPACE` env, typically
    /// set via the downward API). Set `WATCH_ALL_NAMESPACES=true` to opt into
    /// cluster-scoped mode.
    pub watch_namespace: Option<String>,
}

impl Context {
    pub fn new(client: Client) -> Self {
        let image_overrides = load_image_overrides();
        let reporter = Reporter {
            controller: "servarr-operator".into(),
            instance: std::env::var("POD_NAME").ok(),
        };
        let watch_all = std::env::var("WATCH_ALL_NAMESPACES")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let watch_namespace = if watch_all {
            None
        } else {
            std::env::var("WATCH_NAMESPACE")
                .ok()
                .filter(|s| !s.is_empty())
        };
        if let Some(ref ns) = watch_namespace {
            info!(%ns, "namespace-scoped mode");
        } else {
            info!("cluster-scoped mode (watching all namespaces)");
        }
        Self {
            client,
            image_overrides,
            reporter,
            watch_namespace,
        }
    }
}

/// Read DEFAULT_IMAGE_<APP>_REPO and DEFAULT_IMAGE_<APP>_TAG env vars for each known app.
fn load_image_overrides() -> HashMap<String, ImageSpec> {
    let apps = [
        "sonarr",
        "radarr",
        "lidarr",
        "prowlarr",
        "sabnzbd",
        "transmission",
        "tautulli",
        "overseerr",
        "maintainerr",
        "jackett",
    ];

    let mut overrides = HashMap::new();

    for app in &apps {
        let repo_key = format!("DEFAULT_IMAGE_{}_REPO", app.to_uppercase());
        let tag_key = format!("DEFAULT_IMAGE_{}_TAG", app.to_uppercase());

        if let Ok(repo) = std::env::var(&repo_key) {
            let tag = std::env::var(&tag_key).unwrap_or_default();
            info!(%app, %repo, %tag, "loaded image override from env");
            overrides.insert(
                app.to_string(),
                ImageSpec {
                    repository: repo,
                    tag,
                    digest: String::new(),
                    pull_policy: "IfNotPresent".into(),
                },
            );
        }
    }

    overrides
}
