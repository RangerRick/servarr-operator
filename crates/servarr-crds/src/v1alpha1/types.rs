use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageSpec {
    pub repository: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default = "default_pull_policy")]
    pub pull_policy: String,
}

fn default_pull_policy() -> String {
    "IfNotPresent".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PvcVolume {
    pub name: String,
    pub mount_path: String,
    #[serde(default = "default_access_mode")]
    pub access_mode: String,
    #[serde(default = "default_pvc_size")]
    pub size: String,
    #[serde(default)]
    pub storage_class: String,
}

fn default_access_mode() -> String {
    "ReadWriteOnce".to_string()
}

fn default_pvc_size() -> String {
    "1Gi".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NfsMount {
    pub name: String,
    pub server: String,
    pub path: String,
    pub mount_path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersistenceSpec {
    #[serde(default)]
    pub volumes: Vec<PvcVolume>,
    #[serde(default)]
    pub nfs_mounts: Vec<NfsMount>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySpec {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_route_type")]
    pub route_type: RouteType,
    #[serde(default)]
    pub parent_refs: Vec<GatewayParentRef>,
    #[serde(default)]
    pub hosts: Vec<String>,
    /// TLS configuration. When enabled, the controller creates a cert-manager
    /// Certificate and uses a TCPRoute instead of an HTTPRoute.
    #[serde(default)]
    pub tls: Option<TlsSpec>,
}

/// TLS termination via cert-manager.
///
/// When `enabled` is true the operator creates a cert-manager `Certificate`
/// resource referencing the given `cert_issuer` and switches the route type
/// from HTTPRoute to TCPRoute for TLS pass-through.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TlsSpec {
    /// Whether TLS is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Name of the cert-manager ClusterIssuer or Issuer to use.
    #[serde(default)]
    pub cert_issuer: String,
    /// Override for the TLS Secret name. If omitted, derived from the app name.
    #[serde(default)]
    pub secret_name: Option<String>,
}

fn default_route_type() -> RouteType {
    RouteType::Http
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub enum RouteType {
    #[default]
    Http,
    Tcp,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GatewayParentRef {
    pub name: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub section_name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    #[serde(default = "default_service_type")]
    pub service_type: String,
    pub ports: Vec<ServicePort>,
}

fn default_service_type() -> String {
    "ClusterIP".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServicePort {
    pub name: String,
    pub port: i32,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub container_port: Option<i32>,
    #[serde(default)]
    pub host_port: Option<i32>,
}

fn default_protocol() -> String {
    "TCP".to_string()
}

/// Security profile for the container.
///
/// `profileType` selects the security model:
/// - `LinuxServer` (default): s6-overlay images needing CHOWN/SETGID/SETUID.
///   Uses `user`/`group` for PUID/PGID env vars and fsGroup.
/// - `NonRoot`: Images that run as a non-root user natively.
///   Uses `user`/`group` for runAsUser/runAsGroup/fsGroup.
/// - `Custom`: Full control over security context fields.
///   Uses all fields including capabilities, readOnlyRootFilesystem, etc.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecurityProfile {
    #[serde(default)]
    pub profile_type: SecurityProfileType,
    #[serde(default = "default_uid")]
    pub user: i64,
    #[serde(default = "default_uid")]
    pub group: i64,
    /// Override runAsNonRoot. Derived from profile_type if not set.
    #[serde(default)]
    pub run_as_non_root: Option<bool>,
    /// Override readOnlyRootFilesystem (default: false).
    #[serde(default)]
    pub read_only_root_filesystem: Option<bool>,
    /// Override allowPrivilegeEscalation (default: false).
    #[serde(default)]
    pub allow_privilege_escalation: Option<bool>,
    /// Additional Linux capabilities to add.
    #[serde(default)]
    pub capabilities_add: Vec<String>,
    /// Linux capabilities to drop (default: ["ALL"] for LinuxServer/NonRoot).
    #[serde(default)]
    pub capabilities_drop: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub enum SecurityProfileType {
    #[default]
    LinuxServer,
    NonRoot,
    Custom,
}

fn default_uid() -> i64 {
    65534
}

impl SecurityProfile {
    pub fn linux_server(user: i64, group: i64) -> Self {
        Self {
            profile_type: SecurityProfileType::LinuxServer,
            user,
            group,
            ..Default::default()
        }
    }

    pub fn non_root(user: i64, group: i64) -> Self {
        Self {
            profile_type: SecurityProfileType::NonRoot,
            user,
            group,
            ..Default::default()
        }
    }

    pub fn custom() -> Self {
        Self {
            profile_type: SecurityProfileType::Custom,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRequirements {
    #[serde(default)]
    pub limits: ResourceList,
    #[serde(default)]
    pub requests: ResourceList,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceList {
    #[serde(default)]
    pub cpu: String,
    #[serde(default)]
    pub memory: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbeSpec {
    #[serde(default)]
    pub liveness: ProbeConfig,
    #[serde(default)]
    pub readiness: ProbeConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbeConfig {
    #[serde(default)]
    pub probe_type: ProbeType,
    #[serde(default)]
    pub path: String,
    /// Command to run for Exec probes. Ignored for Http/Tcp probe types.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default = "default_initial_delay")]
    pub initial_delay_seconds: i32,
    #[serde(default = "default_period")]
    pub period_seconds: i32,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: i32,
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: i32,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            probe_type: ProbeType::Http,
            path: "/".to_string(),
            command: Vec::new(),
            initial_delay_seconds: 30,
            period_seconds: 10,
            timeout_seconds: 1,
            failure_threshold: 3,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
pub enum ProbeType {
    #[default]
    Http,
    Tcp,
    Exec,
}

fn default_initial_delay() -> i32 {
    30
}
fn default_period() -> i32 {
    10
}
fn default_timeout() -> i32 {
    1
}
fn default_failure_threshold() -> i32 {
    3
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeScheduling {
    #[serde(default)]
    pub node_selector: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    #[schemars(schema_with = "json_object_array_schema")]
    pub tolerations: Vec<serde_json::Value>,
    #[serde(default)]
    #[schemars(schema_with = "json_object_schema")]
    pub affinity: Option<serde_json::Value>,
}

fn default_true() -> bool {
    true
}

/// Configuration for the generated NetworkPolicy.
///
/// Controls egress rules (DNS, internet, private CIDRs) and ingress
/// from the gateway namespace. When omitted, the operator creates a
/// basic ingress-only policy on the app ports.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicyConfig {
    /// Allow pods in the same namespace to reach this app (default: true).
    #[serde(default = "default_true")]
    pub allow_same_namespace: bool,
    /// Allow egress to kube-system DNS (UDP/TCP 53) (default: true).
    #[serde(default = "default_true")]
    pub allow_dns: bool,
    /// Allow egress to the public internet (default: false).
    #[serde(default)]
    pub allow_internet_egress: bool,
    /// CIDR blocks to deny in egress (e.g. RFC 1918 ranges).
    #[serde(default)]
    pub denied_cidr_blocks: Vec<String>,
    /// Arbitrary additional egress rules (raw NetworkPolicyEgressRule JSON).
    #[serde(default)]
    #[schemars(schema_with = "json_object_array_schema")]
    pub custom_egress_rules: Vec<serde_json::Value>,
}

impl Default for NetworkPolicyConfig {
    fn default() -> Self {
        Self {
            allow_same_namespace: true,
            allow_dns: true,
            allow_internet_egress: false,
            denied_cidr_blocks: Vec::new(),
            custom_egress_rules: Vec::new(),
        }
    }
}

/// Configuration for API-driven health checks.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiHealthCheckSpec {
    /// Whether API health checking is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// How often (in seconds) to poll the app API for health. Defaults to 60.
    #[serde(default)]
    pub interval_seconds: Option<u32>,
}

/// Backup configuration for the app.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackupSpec {
    /// Whether automated backups are enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Cron expression for backup schedule (e.g. "0 3 * * *").
    #[serde(default)]
    pub schedule: String,
    /// Number of backups to retain.
    #[serde(default = "default_retention_count")]
    pub retention_count: u32,
}

fn default_retention_count() -> u32 {
    5
}

impl Default for BackupSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule: String::new(),
            retention_count: default_retention_count(),
        }
    }
}

/// GPU device passthrough configuration.
///
/// When set, the corresponding GPU device plugin resource is added
/// to the container's resource limits and requests.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GpuSpec {
    /// NVIDIA GPU count (adds `nvidia.com/gpu` resource limit+request).
    #[serde(default)]
    pub nvidia: Option<i32>,
    /// Intel iGPU count (adds `gpu.intel.com/i915` resource limit+request).
    #[serde(default)]
    pub intel: Option<i32>,
    /// AMD GPU count (adds `amd.com/gpu` resource limit+request).
    #[serde(default)]
    pub amd: Option<i32>,
}

/// Configuration for Prowlarr cross-app synchronization.
///
/// When enabled on a Prowlarr-type ServarrApp, the operator discovers
/// Sonarr/Radarr/Lidarr instances in the target namespace and registers
/// them as applications in Prowlarr for indexer sync.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrSyncSpec {
    /// Whether Prowlarr sync is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Namespace to discover apps in. Defaults to the Prowlarr CR's namespace.
    #[serde(default)]
    pub namespace_scope: Option<String>,
    /// Whether to remove apps from Prowlarr when their CRs are deleted.
    #[serde(default = "default_true")]
    pub auto_remove: bool,
}

impl Default for ProwlarrSyncSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            namespace_scope: None,
            auto_remove: true,
        }
    }
}

/// Configuration for Overseerr cross-app synchronization.
///
/// When enabled on an Overseerr-type ServarrApp, the operator discovers
/// Sonarr/Radarr instances in the target namespace and registers them as
/// servers in Overseerr with correct `is4k`/`isDefault` flags.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OverseerrSyncSpec {
    /// Whether Overseerr sync is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Namespace to discover apps in. Defaults to the Overseerr CR's namespace.
    #[serde(default)]
    pub namespace_scope: Option<String>,
    /// Whether to remove servers from Overseerr when their CRs are deleted.
    #[serde(default = "default_true")]
    pub auto_remove: bool,
}

impl Default for OverseerrSyncSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            namespace_scope: None,
            auto_remove: true,
        }
    }
}

fn json_object_schema(_gen: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "object" })
}

fn json_object_array_schema(_gen: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "array",
        "items": { "type": "object" }
    })
}
