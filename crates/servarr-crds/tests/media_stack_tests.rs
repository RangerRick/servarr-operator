use servarr_crds::*;

// ---------------------------------------------------------------------------
// Tier assignment
// ---------------------------------------------------------------------------

#[test]
fn test_tier_assignment() {
    assert_eq!(AppType::Plex.tier(), 0);
    assert_eq!(AppType::Jellyfin.tier(), 0);
    assert_eq!(AppType::SshBastion.tier(), 0);
    assert_eq!(AppType::Sabnzbd.tier(), 1);
    assert_eq!(AppType::Transmission.tier(), 1);
    assert_eq!(AppType::Sonarr.tier(), 2);
    assert_eq!(AppType::Radarr.tier(), 2);
    assert_eq!(AppType::Lidarr.tier(), 2);
    assert_eq!(AppType::Tautulli.tier(), 3);
    assert_eq!(AppType::Overseerr.tier(), 3);
    assert_eq!(AppType::Maintainerr.tier(), 3);
    assert_eq!(AppType::Prowlarr.tier(), 3);
    assert_eq!(AppType::Jackett.tier(), 3);
}

#[test]
fn test_tier_names() {
    assert_eq!(AppType::tier_name(0), "MediaServers");
    assert_eq!(AppType::tier_name(1), "DownloadClients");
    assert_eq!(AppType::tier_name(2), "MediaManagers");
    assert_eq!(AppType::tier_name(3), "Ancillary");
    assert_eq!(AppType::tier_name(99), "Unknown");
}

// ---------------------------------------------------------------------------
// Child name generation
// ---------------------------------------------------------------------------

#[test]
fn test_child_name_without_instance() {
    let app = StackApp {
        app: AppType::Sonarr,
        instance: None,
        enabled: true,
        image: None,
        uid: None,
        gid: None,
        security: None,
        service: None,
        gateway: None,
        resources: None,
        persistence: None,
        env: Vec::new(),
        probes: None,
        scheduling: None,
        network_policy: None,
        network_policy_config: None,
        app_config: None,
        api_key_secret: None,
        api_health_check: None,
        backup: None,
        image_pull_secrets: None,
        pod_annotations: None,
        gpu: None,
        prowlarr_sync: None,
        overseerr_sync: None,
        split4k: None,
        split4k_overrides: None,
    };
    assert_eq!(app.child_name("media"), "media-sonarr");
}

#[test]
fn test_child_name_with_instance() {
    let app = StackApp {
        app: AppType::Sonarr,
        instance: Some("4k".into()),
        enabled: true,
        image: None,
        uid: None,
        gid: None,
        security: None,
        service: None,
        gateway: None,
        resources: None,
        persistence: None,
        env: Vec::new(),
        probes: None,
        scheduling: None,
        network_policy: None,
        network_policy_config: None,
        app_config: None,
        api_key_secret: None,
        api_health_check: None,
        backup: None,
        image_pull_secrets: None,
        pod_annotations: None,
        gpu: None,
        prowlarr_sync: None,
        overseerr_sync: None,
        split4k: None,
        split4k_overrides: None,
    };
    assert_eq!(app.child_name("stack"), "stack-sonarr-4k");
}

// ---------------------------------------------------------------------------
// Helper to create a minimal StackApp
// ---------------------------------------------------------------------------

fn minimal_stack_app(app: AppType) -> StackApp {
    StackApp {
        app,
        instance: None,
        enabled: true,
        image: None,
        uid: None,
        gid: None,
        security: None,
        service: None,
        gateway: None,
        resources: None,
        persistence: None,
        env: Vec::new(),
        probes: None,
        scheduling: None,
        network_policy: None,
        network_policy_config: None,
        app_config: None,
        api_key_secret: None,
        api_health_check: None,
        backup: None,
        image_pull_secrets: None,
        pod_annotations: None,
        gpu: None,
        prowlarr_sync: None,
        overseerr_sync: None,
        split4k: None,
        split4k_overrides: None,
    }
}

// ---------------------------------------------------------------------------
// Merge: env
// ---------------------------------------------------------------------------

#[test]
fn test_merge_env_app_overrides_stack() {
    let defaults = StackDefaults {
        env: vec![
            EnvVar {
                name: "TZ".into(),
                value: "UTC".into(),
            },
            EnvVar {
                name: "FOO".into(),
                value: "bar".into(),
            },
        ],
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.env = vec![EnvVar {
        name: "TZ".into(),
        value: "America/New_York".into(),
    }];

    let spec = app.to_servarr_spec(Some(&defaults));
    assert_eq!(spec.env.len(), 2);

    let tz = spec.env.iter().find(|e| e.name == "TZ").unwrap();
    assert_eq!(tz.value, "America/New_York");

    let foo = spec.env.iter().find(|e| e.name == "FOO").unwrap();
    assert_eq!(foo.value, "bar");
}

// ---------------------------------------------------------------------------
// Merge: persistence
// ---------------------------------------------------------------------------

#[test]
fn test_merge_persistence_app_pvc_replaces_stack() {
    let defaults = StackDefaults {
        persistence: Some(PersistenceSpec {
            volumes: vec![PvcVolume {
                name: "config".into(),
                mount_path: "/config".into(),
                size: "1Gi".into(),
                ..Default::default()
            }],
            nfs_mounts: Vec::new(),
        }),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.persistence = Some(PersistenceSpec {
        volumes: vec![PvcVolume {
            name: "data".into(),
            mount_path: "/data".into(),
            size: "10Gi".into(),
            ..Default::default()
        }],
        nfs_mounts: Vec::new(),
    });

    let spec = app.to_servarr_spec(Some(&defaults));
    let p = spec.persistence.unwrap();
    assert_eq!(p.volumes.len(), 1);
    assert_eq!(p.volumes[0].name, "data");
}

#[test]
fn test_merge_persistence_nfs_additive_dedup() {
    let defaults = StackDefaults {
        persistence: Some(PersistenceSpec {
            volumes: Vec::new(),
            nfs_mounts: vec![
                NfsMount {
                    name: "media".into(),
                    server: "192.168.1.100".into(),
                    path: "/exports/media".into(),
                    mount_path: "/media".into(),
                    read_only: false,
                },
                NfsMount {
                    name: "shared".into(),
                    server: "192.168.1.100".into(),
                    path: "/exports/shared".into(),
                    mount_path: "/shared".into(),
                    read_only: true,
                },
            ],
        }),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.persistence = Some(PersistenceSpec {
        volumes: Vec::new(),
        nfs_mounts: vec![NfsMount {
            name: "media".into(),
            server: "10.0.0.1".into(),
            path: "/nfs/media".into(),
            mount_path: "/media".into(),
            read_only: true,
        }],
    });

    let spec = app.to_servarr_spec(Some(&defaults));
    let p = spec.persistence.unwrap();
    assert_eq!(p.nfs_mounts.len(), 2);

    let media = p.nfs_mounts.iter().find(|m| m.name == "media").unwrap();
    assert_eq!(media.server, "10.0.0.1"); // per-app wins
    assert!(media.read_only);

    let shared = p.nfs_mounts.iter().find(|m| m.name == "shared").unwrap();
    assert_eq!(shared.server, "192.168.1.100"); // stack default preserved
}

// ---------------------------------------------------------------------------
// to_servarr_spec: no defaults, with defaults, with overrides
// ---------------------------------------------------------------------------

#[test]
fn test_to_servarr_spec_no_defaults() {
    let mut app = minimal_stack_app(AppType::Radarr);
    app.uid = Some(1000);

    let spec = app.to_servarr_spec(None);
    assert!(matches!(spec.app, AppType::Radarr));
    assert_eq!(spec.uid, Some(1000));
    assert!(spec.gid.is_none());
    assert!(spec.security.is_none());
}

#[test]
fn test_to_servarr_spec_with_defaults() {
    let defaults = StackDefaults {
        uid: Some(568),
        gid: Some(568),
        network_policy: Some(true),
        ..Default::default()
    };

    let app = minimal_stack_app(AppType::Sonarr);
    let spec = app.to_servarr_spec(Some(&defaults));
    assert_eq!(spec.uid, Some(568));
    assert_eq!(spec.gid, Some(568));
    assert_eq!(spec.network_policy, Some(true));
}

#[test]
fn test_to_servarr_spec_with_overrides() {
    let defaults = StackDefaults {
        uid: Some(568),
        gid: Some(568),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.uid = Some(1000);

    let spec = app.to_servarr_spec(Some(&defaults));
    assert_eq!(spec.uid, Some(1000)); // per-app override
    assert_eq!(spec.gid, Some(568)); // stack default
}

// ---------------------------------------------------------------------------
// CRD serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn test_media_stack_serde_roundtrip() {
    let spec = MediaStackSpec {
        defaults: Some(StackDefaults {
            uid: Some(568),
            gid: Some(568),
            env: vec![EnvVar {
                name: "TZ".into(),
                value: "UTC".into(),
            }],
            ..Default::default()
        }),
        apps: vec![
            StackApp {
                app: AppType::Jellyfin,
                instance: None,
                enabled: true,
                image: None,
                uid: None,
                gid: None,
                security: None,
                service: None,
                gateway: None,
                resources: None,
                persistence: None,
                env: Vec::new(),
                probes: None,
                scheduling: None,
                network_policy: None,
                network_policy_config: None,
                app_config: None,
                api_key_secret: None,
                api_health_check: None,
                backup: None,
                image_pull_secrets: None,
                pod_annotations: None,
                gpu: None,
                prowlarr_sync: None,
                overseerr_sync: None,
                split4k: None,
                split4k_overrides: None,
            },
            StackApp {
                app: AppType::Sonarr,
                instance: Some("4k".into()),
                enabled: true,
                uid: Some(1000),
                image: None,
                gid: None,
                security: None,
                service: None,
                gateway: None,
                resources: None,
                persistence: None,
                env: Vec::new(),
                probes: None,
                scheduling: None,
                network_policy: None,
                network_policy_config: None,
                app_config: None,
                api_key_secret: None,
                api_health_check: None,
                backup: None,
                image_pull_secrets: None,
                pod_annotations: None,
                gpu: None,
                prowlarr_sync: None,
                overseerr_sync: None,
                split4k: None,
                split4k_overrides: None,
            },
        ],
    };

    let json = serde_json::to_string_pretty(&spec).unwrap();
    let deserialized: MediaStackSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.apps.len(), 2);
    assert_eq!(deserialized.apps[0].app, AppType::Jellyfin);
    assert_eq!(deserialized.apps[1].instance.as_deref(), Some("4k"));
    assert_eq!(deserialized.apps[1].uid, Some(1000));
}

// ---------------------------------------------------------------------------
// CRD generation and structural schema validity
// ---------------------------------------------------------------------------

#[test]
fn test_media_stack_crd_generation() {
    use kube::CustomResourceExt;
    let crd = MediaStack::crd();
    let yaml = serde_yaml::to_string(&crd).unwrap();
    assert!(yaml.contains("MediaStack"));
    assert!(yaml.contains("servarr.dev"));
    assert!(yaml.contains("v1alpha1"));
}

#[test]
fn test_media_stack_crd_schema_structural_validity() {
    use kube::CustomResourceExt;

    let crd = MediaStack::crd();
    let json = serde_json::to_value(&crd).unwrap();

    let mut violations = Vec::new();
    check_no_nullable_in_any_of(&json, "$", &mut violations);

    assert!(
        violations.is_empty(),
        "MediaStack CRD schema has Kubernetes structural violations:\n{}",
        violations.join("\n")
    );
}

fn check_no_nullable_in_any_of(
    value: &serde_json::Value,
    path: &str,
    violations: &mut Vec<String>,
) {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return,
    };

    for keyword in ["anyOf", "oneOf"] {
        if let Some(variants) = obj.get(keyword).and_then(|v| v.as_array()) {
            for (i, variant) in variants.iter().enumerate() {
                let variant_path = format!("{path}.{keyword}[{i}]");
                if variant.get("nullable").and_then(|v| v.as_bool()) == Some(true) {
                    violations.push(format!(
                        "{variant_path}: nullable must not appear inside {keyword}"
                    ));
                }
                check_no_nullable_in_any_of(variant, &variant_path, violations);
            }
        }
    }

    if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
        for (key, val) in props {
            check_no_nullable_in_any_of(val, &format!("{path}.properties.{key}"), violations);
        }
    }
    if let Some(items) = obj.get("items") {
        check_no_nullable_in_any_of(items, &format!("{path}.items"), violations);
    }
    if let Some(additional) = obj.get("additionalProperties") {
        check_no_nullable_in_any_of(
            additional,
            &format!("{path}.additionalProperties"),
            violations,
        );
    }

    if let Some(versions) = obj.get("versions").and_then(|v| v.as_array()) {
        for (i, ver) in versions.iter().enumerate() {
            if let Some(schema) = ver.get("schema") {
                check_no_nullable_in_any_of(
                    schema,
                    &format!("{path}.versions[{i}].schema"),
                    violations,
                );
            }
        }
    }
    if let Some(schema) = obj.get("openAPIV3Schema") {
        check_no_nullable_in_any_of(schema, &format!("{path}.openAPIV3Schema"), violations);
    }
}

// ---------------------------------------------------------------------------
// StackPhase display
// ---------------------------------------------------------------------------

#[test]
fn test_stack_phase_display() {
    assert_eq!(StackPhase::Pending.to_string(), "Pending");
    assert_eq!(StackPhase::RollingOut.to_string(), "RollingOut");
    assert_eq!(StackPhase::Ready.to_string(), "Ready");
    assert_eq!(StackPhase::Degraded.to_string(), "Degraded");
}

// ---------------------------------------------------------------------------
// Pod annotations merge
// ---------------------------------------------------------------------------

#[test]
fn test_merge_pod_annotations() {
    let defaults = StackDefaults {
        pod_annotations: Some(std::collections::BTreeMap::from([
            ("prometheus.io/scrape".into(), "true".into()),
            ("example.com/team".into(), "media".into()),
        ])),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.pod_annotations = Some(std::collections::BTreeMap::from([(
        "prometheus.io/scrape".into(),
        "false".into(),
    )]));

    let spec = app.to_servarr_spec(Some(&defaults));
    let annotations = spec.pod_annotations.unwrap();
    assert_eq!(annotations["prometheus.io/scrape"], "false"); // per-app wins
    assert_eq!(annotations["example.com/team"], "media"); // stack default preserved
}

// ---------------------------------------------------------------------------
// split4k: expand()
// ---------------------------------------------------------------------------

#[test]
fn test_expand_no_split4k_produces_one_entry() {
    let app = minimal_stack_app(AppType::Sonarr);
    let result = app.expand("media", None).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "media-sonarr");
    assert!(result[0].1.instance.is_none());
}

#[test]
fn test_expand_split4k_false_produces_one_entry() {
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(false);
    let result = app.expand("media", None).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "media-sonarr");
}

#[test]
fn test_expand_split4k_true_produces_two_entries() {
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);

    let result = app.expand("media", None).unwrap();
    assert_eq!(result.len(), 2);

    // Base instance
    assert_eq!(result[0].0, "media-sonarr");
    assert!(result[0].1.instance.is_none());

    // 4K instance
    assert_eq!(result[1].0, "media-sonarr-4k");
    assert_eq!(result[1].1.instance.as_deref(), Some("4k"));
}

#[test]
fn test_expand_split4k_radarr() {
    let mut app = minimal_stack_app(AppType::Radarr);
    app.split4k = Some(true);

    let result = app.expand("stack", None).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].0, "stack-radarr");
    assert_eq!(result[1].0, "stack-radarr-4k");
    assert_eq!(result[1].1.instance.as_deref(), Some("4k"));
}

#[test]
fn test_expand_split4k_invalid_app_type() {
    let mut app = minimal_stack_app(AppType::Prowlarr);
    app.split4k = Some(true);

    let result = app.expand("media", None);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("prowlarr"));
}

#[test]
fn test_expand_split4k_invalid_overseerr() {
    let mut app = minimal_stack_app(AppType::Overseerr);
    app.split4k = Some(true);

    let result = app.expand("media", None);
    assert!(result.is_err());
}

#[test]
fn test_expand_split4k_overrides_env() {
    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);
    app.env = vec![EnvVar {
        name: "TZ".into(),
        value: "UTC".into(),
    }];
    app.split4k_overrides = Some(Split4kOverrides {
        env: vec![EnvVar {
            name: "QUALITY".into(),
            value: "4k".into(),
        }],
        ..Default::default()
    });

    let result = app.expand("media", None).unwrap();
    assert_eq!(result.len(), 2);

    // Base should have only TZ
    assert_eq!(result[0].1.env.len(), 1);
    assert_eq!(result[0].1.env[0].name, "TZ");

    // 4K should have both TZ and QUALITY
    assert_eq!(result[1].1.env.len(), 2);
    assert!(result[1].1.env.iter().any(|e| e.name == "TZ"));
    assert!(result[1].1.env.iter().any(|e| e.name == "QUALITY"));
}

#[test]
fn test_expand_split4k_overrides_resources() {
    let mut app = minimal_stack_app(AppType::Radarr);
    app.split4k = Some(true);
    app.split4k_overrides = Some(Split4kOverrides {
        resources: Some(ResourceRequirements {
            limits: ResourceList {
                cpu: "4".into(),
                memory: "2Gi".into(),
            },
            requests: ResourceList {
                cpu: "500m".into(),
                memory: "512Mi".into(),
            },
        }),
        ..Default::default()
    });

    let result = app.expand("media", None).unwrap();

    // Base has no resources
    assert!(result[0].1.resources.is_none());

    // 4K has override resources
    let r = result[1].1.resources.as_ref().unwrap();
    assert_eq!(r.limits.cpu, "4");
    assert_eq!(r.limits.memory, "2Gi");
}

#[test]
fn test_split4k_valid_only_sonarr_radarr() {
    assert!(minimal_stack_app(AppType::Sonarr).split4k_valid());
    assert!(minimal_stack_app(AppType::Radarr).split4k_valid());
    assert!(!minimal_stack_app(AppType::Lidarr).split4k_valid());
    assert!(!minimal_stack_app(AppType::Prowlarr).split4k_valid());
    assert!(!minimal_stack_app(AppType::Overseerr).split4k_valid());
    assert!(!minimal_stack_app(AppType::Transmission).split4k_valid());
    assert!(!minimal_stack_app(AppType::Plex).split4k_valid());
}

#[test]
fn test_expand_with_stack_defaults() {
    let defaults = StackDefaults {
        uid: Some(1000),
        gid: Some(1000),
        ..Default::default()
    };

    let mut app = minimal_stack_app(AppType::Sonarr);
    app.split4k = Some(true);

    let result = app.expand("media", Some(&defaults)).unwrap();
    assert_eq!(result.len(), 2);

    // Both instances inherit defaults
    assert_eq!(result[0].1.uid, Some(1000));
    assert_eq!(result[1].1.uid, Some(1000));
    assert_eq!(result[1].1.instance.as_deref(), Some("4k"));
}
