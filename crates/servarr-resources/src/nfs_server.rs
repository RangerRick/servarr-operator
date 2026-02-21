use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, PersistentVolumeClaim, PersistentVolumeClaimSpec,
    PodSpec, PodTemplateSpec, SecurityContext, VolumeMount, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::{
    api::core::v1::{Service, ServicePort, ServiceSpec},
    apimachinery::pkg::util::intstr::IntOrString,
};
use servarr_crds::NfsServerSpec;
use std::collections::BTreeMap;

const MANAGED_BY: &str = "servarr-operator";
const NFS_PORT: i32 = 2049;
const COMPONENT: &str = "nfs-server";
const DEFAULT_IMAGE: &str = "itsthenetwork/nfs-server-alpine:12";
const EXPORT_DIR: &str = "/nfsshare";
const DATA_VOLUME: &str = "data";

fn resource_name(stack_name: &str) -> String {
    format!("{stack_name}-nfs-server")
}

fn labels(stack_name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("servarr.dev/stack".into(), stack_name.to_string()),
        ("servarr.dev/component".into(), COMPONENT.to_string()),
        ("app.kubernetes.io/managed-by".into(), MANAGED_BY.into()),
    ])
}

fn selector_labels(stack_name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("servarr.dev/stack".into(), stack_name.to_string()),
        ("servarr.dev/component".into(), COMPONENT.to_string()),
    ])
}

/// Build the StatefulSet for the in-cluster NFS server.
///
/// The StatefulSet runs a single NFS server pod backed by a PVC whose size
/// and storage class are taken from `nfs`. The pod exports `EXPORT_DIR` via
/// NFS on port 2049.
pub fn build_statefulset(
    stack_name: &str,
    namespace: &str,
    nfs: &NfsServerSpec,
    owner_ref: OwnerReference,
) -> StatefulSet {
    let name = resource_name(stack_name);
    let labels = labels(stack_name);
    let selector = selector_labels(stack_name);

    let image = nfs
        .image
        .as_ref()
        .map(|img| {
            let tag = if img.tag.is_empty() {
                "latest".to_string()
            } else {
                img.tag.clone()
            };
            format!("{}:{tag}", img.repository)
        })
        .unwrap_or_else(|| DEFAULT_IMAGE.to_string());

    let storage_class = nfs.storage_class.clone().filter(|s| !s.is_empty());

    let volume_claim_template = PersistentVolumeClaim {
        metadata: ObjectMeta {
            name: Some(DATA_VOLUME.to_string()),
            ..Default::default()
        },
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".to_string()]),
            storage_class_name: storage_class,
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".to_string(),
                    Quantity(nfs.storage_size.clone()),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    StatefulSet {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
        spec: Some(StatefulSetSpec {
            replicas: Some(1),
            service_name: Some(name),
            selector: LabelSelector {
                match_labels: Some(selector.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(selector),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: COMPONENT.to_string(),
                        image: Some(image),
                        image_pull_policy: Some("IfNotPresent".to_string()),
                        env: Some(vec![EnvVar {
                            name: "SHARED_DIRECTORY".to_string(),
                            value: Some(EXPORT_DIR.to_string()),
                            ..Default::default()
                        }]),
                        ports: Some(vec![ContainerPort {
                            name: Some("nfs".to_string()),
                            container_port: NFS_PORT,
                            protocol: Some("TCP".to_string()),
                            ..Default::default()
                        }]),
                        security_context: Some(SecurityContext {
                            privileged: Some(true),
                            ..Default::default()
                        }),
                        volume_mounts: Some(vec![VolumeMount {
                            name: DATA_VOLUME.to_string(),
                            mount_path: EXPORT_DIR.to_string(),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            volume_claim_templates: Some(vec![volume_claim_template]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Build the headless Service for the in-cluster NFS server.
///
/// Other pods reach the NFS server via the cluster-local DNS name
/// `{stack-name}-nfs-server.{namespace}.svc.cluster.local` on port 2049.
pub fn build_service(
    stack_name: &str,
    namespace: &str,
    owner_ref: OwnerReference,
) -> Service {
    let name = resource_name(stack_name);

    Service {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(namespace.to_string()),
            labels: Some(labels(stack_name)),
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            type_: Some("ClusterIP".to_string()),
            selector: Some(selector_labels(stack_name)),
            ports: Some(vec![ServicePort {
                name: Some("nfs".to_string()),
                port: NFS_PORT,
                target_port: Some(IntOrString::Int(NFS_PORT)),
                protocol: Some("TCP".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}
