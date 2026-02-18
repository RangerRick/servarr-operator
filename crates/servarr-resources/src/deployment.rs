use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec, DeploymentStrategy};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMapVolumeSource, Container, ContainerPort, EnvVar, EnvVarSource,
    ExecAction, HTTPGetAction, LocalObjectReference, NFSVolumeSource,
    PersistentVolumeClaimVolumeSource, PodSecurityContext, PodSpec, PodTemplateSpec, Probe,
    ResourceRequirements as K8sResources, SeccompProfile, SecretKeySelector, SecurityContext,
    TCPSocketAction, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use servarr_crds::*;
use std::collections::{BTreeMap, HashMap};

use crate::common;

/// Compute a SHA-256 checksum of any config data that should trigger a pod restart.
pub fn config_checksum(app: &ServarrApp) -> Option<String> {
    use sha2::{Digest, Sha256};

    // Collect all ConfigMaps that should trigger a rollout
    let config_maps = [
        crate::configmap::build(app),
        crate::configmap::build_prowlarr_definitions(app),
    ];

    let mut hasher = Sha256::new();
    let mut has_data = false;

    for cm in config_maps.iter().flatten() {
        if let Some(data) = cm.data.as_ref() {
            let mut keys: Vec<_> = data.keys().collect();
            keys.sort();
            for key in keys {
                hasher.update(key.as_bytes());
                hasher.update(data[key].as_bytes());
            }
            has_data = true;
        }
    }

    has_data.then(|| format!("{:x}", hasher.finalize()))
}

pub fn build(app: &ServarrApp, image_overrides: &HashMap<String, ImageSpec>) -> Deployment {
    let mut defaults = AppDefaults::for_app(&app.spec.app);

    // Apply image override from operator config (env vars / Helm values)
    let app_key = app.spec.app.to_string();
    if let Some(override_image) = image_overrides.get(&app_key) {
        defaults.image = override_image.clone();
    }

    let name = common::app_name(app);
    let ns = common::app_namespace(app);
    let labels = common::labels(app);
    let selector_labels = common::selector_labels(app);

    // CR-level image spec takes highest priority, then env override, then compiled default
    let image_spec = app.spec.image.as_ref().unwrap_or(&defaults.image);
    let security = app.spec.security.as_ref().unwrap_or(&defaults.security);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let resources = app.spec.resources.as_ref().unwrap_or(&defaults.resources);
    let persistence = app
        .spec
        .persistence
        .as_ref()
        .unwrap_or(&defaults.persistence);
    let probes = app.spec.probes.as_ref().unwrap_or(&defaults.probes);
    let uid = app.spec.uid.unwrap_or(defaults.uid);
    let gid = app.spec.gid.unwrap_or(defaults.gid);

    let image = if !image_spec.digest.is_empty() {
        format!("{}@{}", image_spec.repository, image_spec.digest)
    } else {
        format!("{}:{}", image_spec.repository, image_spec.tag)
    };

    let container_ports = build_container_ports(svc_spec, app);
    let has_host_port = container_ports.iter().any(|p| p.host_port.is_some());
    let volume_mounts = build_volume_mounts(persistence, app);
    let volumes = build_volumes(app, persistence);
    let env_vars = build_env_vars(app, &defaults, uid, gid);
    let (container_security, pod_security) = build_security_contexts(security, uid, gid);

    // Auto-select exec probes for Transmission with auth enabled
    let effective_probes = maybe_override_probes_for_auth(app, probes);
    let liveness = build_probe(&effective_probes.liveness, svc_spec);
    let readiness = build_probe(&effective_probes.readiness, svc_spec);
    let startup = build_startup_probe(&effective_probes.liveness, svc_spec);

    let mut limits = BTreeMap::from([
        ("cpu".into(), Quantity(resources.limits.cpu.clone())),
        ("memory".into(), Quantity(resources.limits.memory.clone())),
    ]);
    let mut requests = BTreeMap::from([
        ("cpu".into(), Quantity(resources.requests.cpu.clone())),
        ("memory".into(), Quantity(resources.requests.memory.clone())),
    ]);

    // Merge GPU device plugin resources (limits == requests required by device plugins)
    if let Some(ref gpu) = app.spec.gpu {
        if let Some(n) = gpu.nvidia.filter(|&n| n > 0) {
            let q = Quantity(n.to_string());
            limits.insert("nvidia.com/gpu".into(), q.clone());
            requests.insert("nvidia.com/gpu".into(), q);
        }
        if let Some(n) = gpu.intel.filter(|&n| n > 0) {
            let q = Quantity(n.to_string());
            limits.insert("gpu.intel.com/i915".into(), q.clone());
            requests.insert("gpu.intel.com/i915".into(), q);
        }
        if let Some(n) = gpu.amd.filter(|&n| n > 0) {
            let q = Quantity(n.to_string());
            limits.insert("amd.com/gpu".into(), q.clone());
            requests.insert("amd.com/gpu".into(), q);
        }
    }

    let k8s_resources = K8sResources {
        limits: Some(limits),
        requests: Some(requests),
        ..Default::default()
    };

    let mut init_containers = build_init_containers(app, &image, &container_security);
    if matches!(app.spec.app, AppType::SshBastion) {
        build_ssh_bastion_init_containers(&mut init_containers, app, &image, &container_security);
    }

    let container = Container {
        name: app.spec.app.to_string(),
        image: Some(image.clone()),
        image_pull_policy: Some(image_spec.pull_policy.clone()),
        ports: Some(container_ports),
        env: Some(env_vars),
        volume_mounts: Some(volume_mounts),
        resources: Some(k8s_resources),
        security_context: Some(container_security),
        liveness_probe: Some(liveness),
        readiness_probe: Some(readiness),
        startup_probe: Some(startup),
        ..Default::default()
    };

    let mut pod_spec = PodSpec {
        automount_service_account_token: Some(false),
        security_context: Some(pod_security),
        containers: vec![container],
        volumes: Some(volumes),
        ..Default::default()
    };

    if !init_containers.is_empty() {
        pod_spec.init_containers = Some(init_containers);
    }

    if let Some(ref secrets) = app.spec.image_pull_secrets {
        pod_spec.image_pull_secrets = Some(
            secrets
                .iter()
                .map(|s| LocalObjectReference { name: s.clone() })
                .collect(),
        );
    }

    if let Some(scheduling) = &app.spec.scheduling
        && !scheduling.node_selector.is_empty()
    {
        pod_spec.node_selector = Some(scheduling.node_selector.clone());
    }

    let strategy = if has_host_port {
        Some(DeploymentStrategy {
            type_: Some("Recreate".to_string()),
            ..Default::default()
        })
    } else {
        None
    };

    Deployment {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(ns),
            labels: Some(labels.clone()),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(1),
            strategy,
            selector: LabelSelector {
                match_labels: Some(selector_labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some({
                    let mut pod_meta = ObjectMeta {
                        labels: Some(selector_labels),
                        ..Default::default()
                    };
                    let mut annotations = BTreeMap::new();
                    if let Some(checksum) = config_checksum(app) {
                        annotations.insert("servarr.dev/config-checksum".to_string(), checksum);
                    }
                    // Exclude NFS volumes from Velero fs-backup
                    let nfs_volume_names: Vec<String> = persistence
                        .nfs_mounts
                        .iter()
                        .map(|nfs| format!("nfs-{}", nfs.name))
                        .collect();
                    if !nfs_volume_names.is_empty() {
                        annotations.insert(
                            "backup.velero.io/backup-volumes-excludes".to_string(),
                            nfs_volume_names.join(","),
                        );
                    }
                    if let Some(ref user_annotations) = app.spec.pod_annotations {
                        annotations.extend(user_annotations.clone());
                    }
                    if !annotations.is_empty() {
                        pod_meta.annotations = Some(annotations);
                    }
                    pod_meta
                }),
                spec: Some(pod_spec),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn build_container_ports(svc_spec: &ServiceSpec, app: &ServarrApp) -> Vec<ContainerPort> {
    let mut ports: Vec<ContainerPort> = svc_spec
        .ports
        .iter()
        .map(|p| ContainerPort {
            name: Some(p.name.clone()),
            container_port: p.container_port.unwrap_or(p.port),
            protocol: Some(p.protocol.clone()),
            host_port: p.host_port,
            ..Default::default()
        })
        .collect();

    // Transmission peer port
    if let Some(AppConfig::Transmission(ref tc)) = app.spec.app_config
        && let Some(ref peer) = tc.peer_port
    {
        ports.push(ContainerPort {
            name: Some("peer-tcp".into()),
            container_port: peer.port,
            protocol: Some("TCP".into()),
            host_port: if peer.host_port {
                Some(peer.port)
            } else {
                None
            },
            ..Default::default()
        });
        ports.push(ContainerPort {
            name: Some("peer-udp".into()),
            container_port: peer.port,
            protocol: Some("UDP".into()),
            host_port: if peer.host_port {
                Some(peer.port)
            } else {
                None
            },
            ..Default::default()
        });
    }

    ports
}

fn build_volume_mounts(persistence: &PersistenceSpec, app: &ServarrApp) -> Vec<VolumeMount> {
    let mut mounts: Vec<VolumeMount> = persistence
        .volumes
        .iter()
        .map(|v| VolumeMount {
            name: v.name.clone(),
            mount_path: v.mount_path.clone(),
            ..Default::default()
        })
        .collect();

    for nfs in &persistence.nfs_mounts {
        mounts.push(VolumeMount {
            name: format!("nfs-{}", nfs.name),
            mount_path: nfs.mount_path.clone(),
            read_only: Some(nfs.read_only),
            ..Default::default()
        });
    }

    // Transmission watch dir + scripts volume
    if matches!(app.spec.app, AppType::Transmission) {
        mounts.push(VolumeMount {
            name: "watch".into(),
            mount_path: "/watch".into(),
            ..Default::default()
        });
    }

    // Prowlarr custom indexer definitions
    if app
        .spec
        .app_config
        .as_ref()
        .is_some_and(|c| matches!(c, AppConfig::Prowlarr(pc) if !pc.custom_definitions.is_empty()))
    {
        mounts.push(VolumeMount {
            name: "prowlarr-definitions".into(),
            mount_path: "/config/Definitions/Custom".into(),
            read_only: Some(true),
            ..Default::default()
        });
    }

    // SSH bastion: authorized keys per user + optional restricted-rsync script
    if let Some(AppConfig::SshBastion(ref sc)) = app.spec.app_config {
        for user in &sc.users {
            if !user.public_keys.is_empty() {
                mounts.push(VolumeMount {
                    name: format!("authorized-keys-{}", user.name),
                    mount_path: format!("/etc/authorized_keys/{}", user.name),
                    sub_path: Some(user.name.clone()),
                    read_only: Some(true),
                    ..Default::default()
                });
            }
        }
        if sc.mode == SshMode::RestrictedRsync {
            mounts.push(VolumeMount {
                name: "restricted-rsync".into(),
                mount_path: "/usr/local/bin/restricted-rsync".into(),
                sub_path: Some("restricted-rsync.sh".into()),
                read_only: Some(true),
                ..Default::default()
            });
        }
    }

    mounts
}

fn build_volumes(app: &ServarrApp, persistence: &PersistenceSpec) -> Vec<Volume> {
    let mut volumes: Vec<Volume> = persistence
        .volumes
        .iter()
        .map(|v| Volume {
            name: v.name.clone(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: common::child_name(app, &v.name),
                read_only: None,
            }),
            ..Default::default()
        })
        .collect();

    for nfs in &persistence.nfs_mounts {
        volumes.push(Volume {
            name: format!("nfs-{}", nfs.name),
            nfs: Some(NFSVolumeSource {
                server: nfs.server.clone(),
                path: nfs.path.clone(),
                read_only: Some(nfs.read_only),
            }),
            ..Default::default()
        });
    }

    // Transmission ConfigMap + watch dir
    if matches!(app.spec.app, AppType::Transmission) {
        volumes.push(Volume {
            name: "scripts".into(),
            config_map: Some(ConfigMapVolumeSource {
                name: common::app_name(app),
                default_mode: Some(0o755),
                items: Some(vec![
                    k8s_openapi::api::core::v1::KeyToPath {
                        key: "apply-settings.sh".into(),
                        path: "apply-settings.sh".into(),
                        mode: None,
                    },
                    k8s_openapi::api::core::v1::KeyToPath {
                        key: "settings-override.json".into(),
                        path: "settings-override.json".into(),
                        mode: None,
                    },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        });
        volumes.push(Volume {
            name: "watch".into(),
            empty_dir: Some(Default::default()),
            ..Default::default()
        });
    }

    // SABnzbd tar-unpack scripts ConfigMap
    if let Some(AppConfig::Sabnzbd(ref sc)) = app.spec.app_config
        && sc.tar_unpack
    {
        volumes.push(Volume {
            name: "tar-unpack-scripts".into(),
            config_map: Some(ConfigMapVolumeSource {
                name: common::child_name(app, "tar-unpack"),
                default_mode: Some(0o755),
                ..Default::default()
            }),
            ..Default::default()
        });
    }

    // SABnzbd ConfigMap for host_whitelist
    if matches!(app.spec.app, AppType::Sabnzbd)
        && app
            .spec
            .app_config
            .as_ref()
            .is_some_and(|c| matches!(c, AppConfig::Sabnzbd(sc) if !sc.host_whitelist.is_empty()))
    {
        volumes.push(Volume {
            name: "sabnzbd-scripts".into(),
            config_map: Some(ConfigMapVolumeSource {
                name: common::child_name(app, "sabnzbd-config"),
                default_mode: Some(0o755),
                ..Default::default()
            }),
            ..Default::default()
        });
    }

    // Prowlarr custom indexer definitions ConfigMap
    if app
        .spec
        .app_config
        .as_ref()
        .is_some_and(|c| matches!(c, AppConfig::Prowlarr(pc) if !pc.custom_definitions.is_empty()))
    {
        volumes.push(Volume {
            name: "prowlarr-definitions".into(),
            config_map: Some(ConfigMapVolumeSource {
                name: common::child_name(app, "prowlarr-definitions"),
                ..Default::default()
            }),
            ..Default::default()
        });
    }

    // SSH bastion: authorized-keys Secret (per user) + restricted-rsync ConfigMap
    if let Some(AppConfig::SshBastion(ref sc)) = app.spec.app_config {
        use k8s_openapi::api::core::v1::SecretVolumeSource;
        let secret_name = common::child_name(app, "authorized-keys");
        for user in &sc.users {
            if !user.public_keys.is_empty() {
                volumes.push(Volume {
                    name: format!("authorized-keys-{}", user.name),
                    secret: Some(SecretVolumeSource {
                        secret_name: Some(secret_name.clone()),
                        items: Some(vec![k8s_openapi::api::core::v1::KeyToPath {
                            key: user.name.clone(),
                            path: user.name.clone(),
                            mode: Some(0o444),
                        }]),
                        default_mode: Some(0o444),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
            }
        }
        if sc.mode == SshMode::RestrictedRsync {
            volumes.push(Volume {
                name: "restricted-rsync".into(),
                config_map: Some(ConfigMapVolumeSource {
                    name: common::child_name(app, "restricted-rsync"),
                    default_mode: Some(0o755),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
    }

    volumes
}

fn build_env_vars(app: &ServarrApp, defaults: &AppDefaults, uid: i64, gid: i64) -> Vec<EnvVar> {
    let mut env = Vec::new();

    // LinuxServer PUID/PGID
    if matches!(
        app.spec
            .security
            .as_ref()
            .unwrap_or(&defaults.security)
            .profile_type,
        SecurityProfileType::LinuxServer
    ) {
        env.push(EnvVar {
            name: "PUID".into(),
            value: Some(uid.to_string()),
            ..Default::default()
        });
        env.push(EnvVar {
            name: "PGID".into(),
            value: Some(gid.to_string()),
            ..Default::default()
        });
    }

    // Default env from app defaults
    for e in &defaults.env {
        env.push(EnvVar {
            name: e.name.clone(),
            value: Some(e.value.clone()),
            ..Default::default()
        });
    }

    // User-specified env vars (override defaults)
    for e in &app.spec.env {
        // Remove any default with same name
        env.retain(|existing| existing.name != e.name);
        env.push(EnvVar {
            name: e.name.clone(),
            value: Some(e.value.clone()),
            ..Default::default()
        });
    }

    // SSH bastion env vars
    if let Some(AppConfig::SshBastion(ref sc)) = app.spec.app_config {
        // SSH_USERS: user1:uid1:gid1:shell,user2:uid2:gid2:shell
        let ssh_users: Vec<String> = sc
            .users
            .iter()
            .map(|u| {
                let shell = u.shell.clone().unwrap_or_else(|| {
                    if sc.mode == SshMode::RestrictedRsync {
                        "/usr/local/bin/restricted-rsync".to_string()
                    } else {
                        "/bin/sh".to_string()
                    }
                });
                format!("{}:{}:{}:{}", u.name, u.uid, u.gid, shell)
            })
            .collect();
        env.push(EnvVar {
            name: "SSH_USERS".into(),
            value: Some(ssh_users.join(",")),
            ..Default::default()
        });

        if !sc.enable_password_auth {
            env.push(EnvVar {
                name: "SSH_ENABLE_PASSWORD_AUTH".into(),
                value: Some("false".into()),
                ..Default::default()
            });
        }
        if sc.tcp_forwarding {
            env.push(EnvVar {
                name: "TCP_FORWARDING".into(),
                value: Some("true".into()),
                ..Default::default()
            });
        }
        if sc.gateway_ports {
            env.push(EnvVar {
                name: "GATEWAY_PORTS".into(),
                value: Some("true".into()),
                ..Default::default()
            });
        }
        if sc.disable_sftp {
            env.push(EnvVar {
                name: "SFTP_MODE".into(),
                value: Some("false".into()),
                ..Default::default()
            });
        }
        if sc.sftp_chroot != "%h" {
            env.push(EnvVar {
                name: "SFTP_CHROOT".into(),
                value: Some(sc.sftp_chroot.clone()),
                ..Default::default()
            });
        }
        if !sc.motd.is_empty() {
            env.push(EnvVar {
                name: "MOTD".into(),
                value: Some(sc.motd.clone()),
                ..Default::default()
            });
        }
    }

    // Transmission auth from secret
    if let Some(AppConfig::Transmission(ref tc)) = app.spec.app_config
        && let Some(ref auth) = tc.auth
    {
        env.push(EnvVar {
            name: "USER".into(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: auth.secret_name.clone(),
                    key: "USER".into(),
                    optional: Some(false),
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
        env.push(EnvVar {
            name: "PASS".into(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: auth.secret_name.clone(),
                    key: "PASS".into(),
                    optional: Some(false),
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
    }

    env
}

fn build_security_contexts(
    profile: &SecurityProfile,
    _uid: i64,
    gid: i64,
) -> (SecurityContext, PodSecurityContext) {
    match profile.profile_type {
        SecurityProfileType::LinuxServer => (
            SecurityContext {
                allow_privilege_escalation: Some(false),
                read_only_root_filesystem: Some(false),
                run_as_non_root: Some(false),
                capabilities: Some(Capabilities {
                    drop: Some(vec!["ALL".into()]),
                    add: Some(vec![
                        "CHOWN".into(),
                        "FOWNER".into(),
                        "SETGID".into(),
                        "SETUID".into(),
                    ]),
                }),
                ..Default::default()
            },
            PodSecurityContext {
                fs_group: Some(gid),
                seccomp_profile: Some(SeccompProfile {
                    type_: "RuntimeDefault".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        SecurityProfileType::NonRoot => (
            SecurityContext {
                allow_privilege_escalation: Some(false),
                read_only_root_filesystem: Some(false),
                run_as_non_root: Some(true),
                run_as_user: Some(profile.user),
                run_as_group: Some(profile.group),
                capabilities: Some(Capabilities {
                    drop: Some(vec!["ALL".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            PodSecurityContext {
                fs_group: Some(profile.group),
                seccomp_profile: Some(SeccompProfile {
                    type_: "RuntimeDefault".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        SecurityProfileType::Custom => {
            let run_as_non_root = profile.run_as_non_root.unwrap_or(true);
            let read_only_root = profile.read_only_root_filesystem.unwrap_or(false);
            let allow_priv_esc = profile.allow_privilege_escalation.unwrap_or(false);
            let run_as_user = if profile.user != 0 {
                Some(profile.user)
            } else {
                None
            };
            let run_as_group = if profile.group != 0 {
                Some(profile.group)
            } else {
                None
            };
            let caps_drop = if profile.capabilities_drop.is_empty() {
                Some(vec!["ALL".into()])
            } else {
                Some(profile.capabilities_drop.clone())
            };
            (
                SecurityContext {
                    allow_privilege_escalation: Some(allow_priv_esc),
                    read_only_root_filesystem: Some(read_only_root),
                    run_as_non_root: Some(run_as_non_root),
                    run_as_user,
                    run_as_group,
                    capabilities: Some(Capabilities {
                        drop: caps_drop,
                        add: if profile.capabilities_add.is_empty() {
                            None
                        } else {
                            Some(profile.capabilities_add.clone())
                        },
                    }),
                    ..Default::default()
                },
                PodSecurityContext {
                    fs_group: run_as_group,
                    seccomp_profile: Some(SeccompProfile {
                        type_: "RuntimeDefault".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
        }
    }
}

fn build_probe(config: &ProbeConfig, svc_spec: &ServiceSpec) -> Probe {
    let first_port = svc_spec
        .ports
        .first()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "http".into());

    let mut probe = Probe {
        initial_delay_seconds: Some(config.initial_delay_seconds),
        period_seconds: Some(config.period_seconds),
        timeout_seconds: Some(config.timeout_seconds),
        failure_threshold: Some(config.failure_threshold),
        ..Default::default()
    };

    match config.probe_type {
        ProbeType::Http => {
            probe.http_get = Some(HTTPGetAction {
                path: Some(config.path.clone()),
                port: IntOrString::String(first_port),
                ..Default::default()
            });
        }
        ProbeType::Tcp => {
            probe.tcp_socket = Some(TCPSocketAction {
                port: IntOrString::String(first_port),
                ..Default::default()
            });
        }
        ProbeType::Exec => {
            probe.exec = Some(ExecAction {
                command: Some(config.command.clone()),
            });
        }
    }

    probe
}

/// Build a startup probe from the liveness config with generous timeouts.
/// This gives containers up to 300s to start before the liveness probe takes over.
fn build_startup_probe(liveness_config: &ProbeConfig, svc_spec: &ServiceSpec) -> Probe {
    let mut probe = build_probe(liveness_config, svc_spec);
    probe.initial_delay_seconds = None; // K8s strips default 0, avoid drift
    probe.period_seconds = Some(10);
    probe.timeout_seconds = Some(5);
    probe.failure_threshold = Some(30);
    probe
}

fn build_init_containers(
    app: &ServarrApp,
    image: &str,
    security_context: &SecurityContext,
) -> Vec<Container> {
    let mut init = Vec::new();

    // Transmission settings apply init container
    if matches!(app.spec.app, AppType::Transmission) {
        init.push(Container {
            name: "apply-settings".into(),
            image: Some(image.to_string()),
            command: Some(vec!["/bin/sh".into(), "/scripts/apply-settings.sh".into()]),
            security_context: Some(security_context.clone()),
            volume_mounts: Some(vec![
                VolumeMount {
                    name: "config".into(),
                    mount_path: "/config".into(),
                    ..Default::default()
                },
                VolumeMount {
                    name: "scripts".into(),
                    mount_path: "/scripts".into(),
                    read_only: Some(true),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        });
    }

    // SABnzbd tar-unpack init container (installs tools)
    if let Some(AppConfig::Sabnzbd(ref sc)) = app.spec.app_config
        && sc.tar_unpack
    {
        init.push(Container {
            name: "install-tar-tools".into(),
            image: Some(image.to_string()),
            command: Some(vec![
                "/bin/sh".into(),
                "/tar-scripts/install-tar-tools.sh".into(),
            ]),
            security_context: Some(security_context.clone()),
            volume_mounts: Some(vec![VolumeMount {
                name: "tar-unpack-scripts".into(),
                mount_path: "/tar-scripts".into(),
                read_only: Some(true),
                ..Default::default()
            }]),
            ..Default::default()
        });
    }

    // SABnzbd host_whitelist init container
    if let Some(AppConfig::Sabnzbd(ref sc)) = app.spec.app_config
        && !sc.host_whitelist.is_empty()
    {
        let whitelist_csv = sc.host_whitelist.join(", ");
        init.push(Container {
            name: "apply-whitelist".into(),
            image: Some(image.to_string()),
            command: Some(vec![
                "/bin/sh".into(),
                "/sabnzbd-scripts/apply-whitelist.sh".into(),
                whitelist_csv,
            ]),
            security_context: Some(security_context.clone()),
            volume_mounts: Some(vec![
                VolumeMount {
                    name: "config".into(),
                    mount_path: "/config".into(),
                    ..Default::default()
                },
                VolumeMount {
                    name: "sabnzbd-scripts".into(),
                    mount_path: "/sabnzbd-scripts".into(),
                    read_only: Some(true),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        });
    }

    init
}

/// Build SSH bastion init containers for host key generation and entry.sh patching.
fn build_ssh_bastion_init_containers(
    init: &mut Vec<Container>,
    app: &ServarrApp,
    image: &str,
    security_context: &SecurityContext,
) {
    let ssh_config = match app.spec.app_config {
        Some(AppConfig::SshBastion(ref sc)) => sc,
        _ => return,
    };

    // Host key generation init container
    let keygen_script = r#"#!/bin/sh
set -e
KEY_DIR="/etc/ssh/keys"
mkdir -p "$KEY_DIR"
# Generate host keys if they don't exist
for type in rsa ecdsa ed25519; do
  key_file="$KEY_DIR/ssh_host_${type}_key"
  if [ ! -f "$key_file" ]; then
    echo "Generating $type host key..."
    ssh-keygen -t "$type" -f "$key_file" -N "" -q
  fi
done
echo "Host keys ready."
"#;

    init.push(Container {
        name: "generate-host-keys".into(),
        image: Some(image.to_string()),
        command: Some(vec!["/bin/sh".into(), "-c".into(), keygen_script.into()]),
        security_context: Some(security_context.clone()),
        volume_mounts: Some(vec![VolumeMount {
            name: "host-keys".into(),
            mount_path: "/etc/ssh/keys".into(),
            ..Default::default()
        }]),
        ..Default::default()
    });

    // Patch entry.sh so authorized_keys are read-only (bind-mounted from Secret)
    // and install restricted-rsync if in that mode
    let mut patch_script = String::from(
        r#"#!/bin/sh
set -e
# Patch entry.sh to skip chown/chmod on authorized_keys (they're read-only mounts)
if [ -f /entry.sh ]; then
  sed -i 's/chmod 600 "$f"/true/g' /entry.sh
  sed -i 's/chown "$user:$user" "$f"/true/g' /entry.sh
fi
"#,
    );

    if ssh_config.mode == SshMode::RestrictedRsync {
        patch_script.push_str(
            r#"# Install rsync for restricted-rsync mode
apk add --no-cache rsync >/dev/null 2>&1 || true
"#,
        );
    }

    init.push(Container {
        name: "patch-entry".into(),
        image: Some(image.to_string()),
        command: Some(vec!["/bin/sh".into(), "-c".into(), patch_script]),
        security_context: Some(security_context.clone()),
        ..Default::default()
    });
}

/// For Transmission with auth enabled, automatically switch to exec probes
/// that use curl with credentials, matching the legacy Helm chart behavior.
fn maybe_override_probes_for_auth(app: &ServarrApp, probes: &ProbeSpec) -> ProbeSpec {
    // Only override if user hasn't explicitly set exec probes already
    if matches!(app.spec.app, AppType::Transmission)
        && app
            .spec
            .app_config
            .as_ref()
            .is_some_and(|c| matches!(c, AppConfig::Transmission(tc) if tc.auth.is_some()))
        && !matches!(probes.liveness.probe_type, ProbeType::Exec)
    {
        let exec_cmd = vec![
            "/bin/sh".into(),
            "-c".into(),
            r#"curl -sf -u "$USER:$PASS" http://localhost:9091/ >/dev/null"#.into(),
        ];
        ProbeSpec {
            liveness: ProbeConfig {
                probe_type: ProbeType::Exec,
                command: exec_cmd.clone(),
                initial_delay_seconds: probes.liveness.initial_delay_seconds,
                period_seconds: probes.liveness.period_seconds,
                timeout_seconds: probes.liveness.timeout_seconds,
                failure_threshold: probes.liveness.failure_threshold,
                ..Default::default()
            },
            readiness: ProbeConfig {
                probe_type: ProbeType::Exec,
                command: exec_cmd,
                initial_delay_seconds: probes.readiness.initial_delay_seconds,
                period_seconds: probes.readiness.period_seconds,
                timeout_seconds: probes.readiness.timeout_seconds,
                failure_threshold: probes.readiness.failure_threshold,
                ..Default::default()
            },
        }
    } else {
        probes.clone()
    }
}
