use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use servarr_crds::*;

fn make_app(app_type: AppType) -> ServarrApp {
    ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-123".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: app_type,
            ..Default::default()
        },
        status: None,
    }
}

#[test]
fn test_deployment_builder_sonarr() {
    let app = make_app(AppType::Sonarr);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());

    assert_eq!(deploy.metadata.name.as_deref(), Some("test-app"));
    assert_eq!(deploy.metadata.namespace.as_deref(), Some("media"));

    let spec = deploy.spec.unwrap();
    assert_eq!(spec.replicas, Some(1));

    let pod_spec = spec.template.spec.unwrap();
    assert_eq!(pod_spec.containers.len(), 1);

    let container = &pod_spec.containers[0];
    assert_eq!(container.name, "sonarr");
    assert_eq!(
        container.image.as_deref(),
        Some("linuxserver/sonarr:4.0.16")
    );

    // Check PUID/PGID env vars for LinuxServer
    let env = container.env.as_ref().unwrap();
    assert!(env.iter().any(|e| e.name == "PUID"));
    assert!(env.iter().any(|e| e.name == "PGID"));
    assert!(env.iter().any(|e| e.name == "TZ"));

    // Check ports
    let ports = container.ports.as_ref().unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].container_port, 8989);

    // Check volume mounts (config + downloads)
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(
        mounts
            .iter()
            .any(|m| m.name == "config" && m.mount_path == "/config")
    );
    assert!(
        mounts
            .iter()
            .any(|m| m.name == "downloads" && m.mount_path == "/downloads")
    );

    // Check security context
    let sec = container.security_context.as_ref().unwrap();
    assert_eq!(sec.run_as_non_root, Some(false));
    assert_eq!(sec.allow_privilege_escalation, Some(false));

    // Check pod security
    let pod_sec = pod_spec.security_context.as_ref().unwrap();
    assert_eq!(pod_sec.fs_group, Some(65534));

    // No init containers for standard apps
    assert!(pod_spec.init_containers.is_none());
}

#[test]
fn test_deployment_builder_maintainerr_nonroot() {
    let app = make_app(AppType::Maintainerr);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());

    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];

    assert_eq!(
        container.image.as_deref(),
        Some("ghcr.io/jorenn92/maintainerr:2.19.0")
    );

    let sec = container.security_context.as_ref().unwrap();
    assert_eq!(sec.run_as_non_root, Some(true));
    assert_eq!(sec.run_as_user, Some(65534));

    // NonRoot apps don't get PUID/PGID
    let env = container.env.as_ref().unwrap();
    assert!(!env.iter().any(|e| e.name == "PUID"));
}

#[test]
fn test_deployment_builder_transmission() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("transmission".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-456".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Transmission,
            app_config: Some(AppConfig::Transmission(TransmissionConfig {
                peer_port: Some(PeerPortConfig {
                    port: 51413,
                    host_port: true,
                    ..Default::default()
                }),
                auth: Some(TransmissionAuth {
                    secret_name: "tx-auth".into(),
                }),
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];

    // Check peer ports added
    let ports = container.ports.as_ref().unwrap();
    assert!(ports.iter().any(|p| p.name.as_deref() == Some("peer-tcp")));
    assert!(ports.iter().any(|p| p.name.as_deref() == Some("peer-udp")));

    // Check auth env from secret
    let env = container.env.as_ref().unwrap();
    let user_env = env.iter().find(|e| e.name == "USER").unwrap();
    assert!(user_env.value_from.is_some());

    // Check watch volume mount
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(mounts.iter().any(|m| m.name == "watch"));

    // Check init container exists
    let init = pod_spec.init_containers.as_ref().unwrap();
    assert_eq!(init.len(), 1);
    assert_eq!(init[0].name, "apply-settings");

    // Check volumes include scripts ConfigMap
    let volumes = pod_spec.volumes.as_ref().unwrap();
    assert!(volumes.iter().any(|v| v.name == "scripts"));
    assert!(volumes.iter().any(|v| v.name == "watch"));
}

#[test]
fn test_service_builder() {
    let app = make_app(AppType::Radarr);
    let svc = servarr_resources::service::build(&app);

    assert_eq!(svc.metadata.name.as_deref(), Some("test-app"));
    assert_eq!(svc.metadata.namespace.as_deref(), Some("media"));

    let spec = svc.spec.unwrap();
    assert_eq!(spec.type_.as_deref(), Some("ClusterIP"));

    let ports = spec.ports.unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].port, 7878);
}

#[test]
fn test_pvc_builder() {
    let app = make_app(AppType::Sonarr);
    let pvcs = servarr_resources::pvc::build_all(&app);

    assert_eq!(pvcs.len(), 2); // config + downloads

    let config_pvc = pvcs
        .iter()
        .find(|p| p.metadata.name.as_deref() == Some("test-app-config"));
    assert!(config_pvc.is_some());

    let downloads_pvc = pvcs
        .iter()
        .find(|p| p.metadata.name.as_deref() == Some("test-app-downloads"));
    assert!(downloads_pvc.is_some());
}

#[test]
fn test_pvc_builder_config_only() {
    let app = make_app(AppType::Prowlarr);
    let pvcs = servarr_resources::pvc::build_all(&app);
    assert_eq!(pvcs.len(), 1);
    assert_eq!(pvcs[0].metadata.name.as_deref(), Some("test-app-config"));
}

#[test]
fn test_networkpolicy_builder() {
    let app = make_app(AppType::Sonarr);
    let np = servarr_resources::networkpolicy::build(&app);

    assert_eq!(np.metadata.name.as_deref(), Some("test-app"));
    let spec = np.spec.unwrap();
    let ingress = spec.ingress.unwrap();
    assert_eq!(ingress.len(), 1);
    let ports = ingress[0].ports.as_ref().unwrap();
    assert_eq!(ports.len(), 1);
}

#[test]
fn test_configmap_builder_transmission() {
    let app = make_app(AppType::Transmission);
    let cm = servarr_resources::configmap::build(&app);
    assert!(cm.is_some());

    let cm = cm.unwrap();
    let data = cm.data.unwrap();
    assert!(data.contains_key("settings-override.json"));
    assert!(data.contains_key("apply-settings.sh"));

    let script = &data["apply-settings.sh"];
    assert!(script.contains("jq"));
    assert!(script.contains("chown"));
}

#[test]
fn test_configmap_builder_non_transmission() {
    let app = make_app(AppType::Sonarr);
    let cm = servarr_resources::configmap::build(&app);
    assert!(cm.is_none());
}

#[test]
fn test_httproute_builder_disabled() {
    let app = make_app(AppType::Sonarr);
    let route = servarr_resources::httproute::build(&app);
    assert!(route.is_none());
}

#[test]
fn test_httproute_builder_enabled() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-789".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            gateway: Some(GatewaySpec {
                enabled: true,
                route_type: RouteType::Http,
                parent_refs: vec![GatewayParentRef {
                    name: "istio-gateway".into(),
                    namespace: "istio-system".into(),
                    section_name: String::new(),
                }],
                hosts: vec!["sonarr.example.com".into()],
                tls: None,
            }),
            ..Default::default()
        },
        status: None,
    };

    let route = servarr_resources::httproute::build(&app);
    assert!(route.is_some());
}

#[test]
fn test_custom_env_override() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-abc".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            env: vec![
                servarr_crds::EnvVar {
                    name: "TZ".into(),
                    value: "America/Chicago".into(),
                },
                servarr_crds::EnvVar {
                    name: "CUSTOM_VAR".into(),
                    value: "custom_value".into(),
                },
            ],
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    let env = container.env.as_ref().unwrap();

    // TZ should be overridden
    let tz = env.iter().find(|e| e.name == "TZ").unwrap();
    assert_eq!(tz.value.as_deref(), Some("America/Chicago"));

    // Custom var should be present
    assert!(env.iter().any(|e| e.name == "CUSTOM_VAR"));

    // Should not have duplicate TZ
    let tz_count = env.iter().filter(|e| e.name == "TZ").count();
    assert_eq!(tz_count, 1);
}

#[test]
fn test_custom_image_override() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-def".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            image: Some(ImageSpec {
                repository: "my-registry/sonarr".into(),
                tag: "custom".into(),
                digest: String::new(),
                pull_policy: "Always".into(),
            }),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    assert_eq!(
        container.image.as_deref(),
        Some("my-registry/sonarr:custom")
    );
    assert_eq!(container.image_pull_policy.as_deref(), Some("Always"));
}

#[test]
fn test_image_digest_override() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-ghi".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            image: Some(ImageSpec {
                repository: "linuxserver/sonarr".into(),
                tag: "ignored".into(),
                digest: "sha256:abc123".into(),
                pull_policy: "IfNotPresent".into(),
            }),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    assert_eq!(
        container.image.as_deref(),
        Some("linuxserver/sonarr@sha256:abc123")
    );
}

#[test]
fn test_nfs_mounts() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-jkl".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            persistence: Some(PersistenceSpec {
                volumes: vec![PvcVolume {
                    name: "config".into(),
                    mount_path: "/config".into(),
                    access_mode: "ReadWriteOnce".into(),
                    size: "1Gi".into(),
                    storage_class: String::new(),
                }],
                nfs_mounts: vec![NfsMount {
                    name: "media".into(),
                    server: "192.168.1.100".into(),
                    path: "/exports/media".into(),
                    mount_path: "/media".into(),
                    read_only: true,
                }],
            }),
            ..Default::default()
        },
        status: None,
    };

    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];

    let mounts = container.volume_mounts.as_ref().unwrap();
    let nfs_mount = mounts.iter().find(|m| m.name == "nfs-media").unwrap();
    assert_eq!(nfs_mount.mount_path, "/media");
    assert_eq!(nfs_mount.read_only, Some(true));

    let volumes = pod_spec.volumes.as_ref().unwrap();
    let nfs_vol = volumes.iter().find(|v| v.name == "nfs-media").unwrap();
    let nfs = nfs_vol.nfs.as_ref().unwrap();
    assert_eq!(nfs.server, "192.168.1.100");
    assert_eq!(nfs.path, "/exports/media");
}

#[test]
fn test_image_override_from_env() {
    let app = make_app(AppType::Sonarr);

    let mut overrides = std::collections::HashMap::new();
    overrides.insert(
        "sonarr".to_string(),
        ImageSpec {
            repository: "custom-registry/sonarr".into(),
            tag: "99.0.0".into(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        },
    );

    let deploy = servarr_resources::deployment::build(&app, &overrides);
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    assert_eq!(
        container.image.as_deref(),
        Some("custom-registry/sonarr:99.0.0")
    );
}

#[test]
fn test_deployment_builder_plex() {
    let app = make_app(AppType::Plex);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());

    let spec = deploy.spec.unwrap();
    let pod_spec = spec.template.spec.unwrap();
    let container = &pod_spec.containers[0];

    assert_eq!(container.name, "plex");
    assert_eq!(container.image.as_deref(), Some("linuxserver/plex:1.41.4"));

    // Check port
    let ports = container.ports.as_ref().unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].container_port, 32400);

    // LinuxServer security: PUID/PGID env vars
    let env = container.env.as_ref().unwrap();
    assert!(env.iter().any(|e| e.name == "PUID"));
    assert!(env.iter().any(|e| e.name == "PGID"));

    // Config-only: single volume mount
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(
        mounts
            .iter()
            .any(|m| m.name == "config" && m.mount_path == "/config")
    );
    assert!(
        !mounts.iter().any(|m| m.name == "downloads"),
        "Plex should not have a downloads volume"
    );

    // LinuxServer security context
    let sec = container.security_context.as_ref().unwrap();
    assert_eq!(sec.run_as_non_root, Some(false));
    assert_eq!(sec.allow_privilege_escalation, Some(false));
}

#[test]
fn test_deployment_builder_jellyfin() {
    let app = make_app(AppType::Jellyfin);
    let deploy = servarr_resources::deployment::build(&app, &std::collections::HashMap::new());

    let spec = deploy.spec.unwrap();
    let pod_spec = spec.template.spec.unwrap();
    let container = &pod_spec.containers[0];

    assert_eq!(container.name, "jellyfin");
    assert_eq!(
        container.image.as_deref(),
        Some("linuxserver/jellyfin:10.10.7")
    );

    // Check port
    let ports = container.ports.as_ref().unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].container_port, 8096);

    // LinuxServer security: PUID/PGID env vars
    let env = container.env.as_ref().unwrap();
    assert!(env.iter().any(|e| e.name == "PUID"));
    assert!(env.iter().any(|e| e.name == "PGID"));

    // Config-only: single volume mount
    let mounts = container.volume_mounts.as_ref().unwrap();
    assert!(
        mounts
            .iter()
            .any(|m| m.name == "config" && m.mount_path == "/config")
    );
    assert!(
        !mounts.iter().any(|m| m.name == "downloads"),
        "Jellyfin should not have a downloads volume"
    );
}

#[test]
fn test_cr_image_overrides_env_override() {
    // CR-level image spec should take priority over env overrides
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-priority".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Sonarr,
            image: Some(ImageSpec {
                repository: "cr-level/sonarr".into(),
                tag: "cr-tag".into(),
                digest: String::new(),
                pull_policy: "Always".into(),
            }),
            ..Default::default()
        },
        status: None,
    };

    let mut overrides = std::collections::HashMap::new();
    overrides.insert(
        "sonarr".to_string(),
        ImageSpec {
            repository: "env-level/sonarr".into(),
            tag: "env-tag".into(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        },
    );

    let deploy = servarr_resources::deployment::build(&app, &overrides);
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];
    // CR-level should win
    assert_eq!(container.image.as_deref(), Some("cr-level/sonarr:cr-tag"));
}
