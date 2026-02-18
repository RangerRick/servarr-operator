use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use servarr_crds::{AppConfig, AppType, ServarrApp};
use std::collections::BTreeMap;

use crate::common;

/// Build an authorized-keys Secret for SSH bastion apps.
///
/// Each user gets a key in the Secret with their public keys.
pub fn build_authorized_keys(app: &ServarrApp) -> Option<Secret> {
    if !matches!(app.spec.app, AppType::SshBastion) {
        return None;
    }

    let ssh_config = match app.spec.app_config {
        Some(AppConfig::SshBastion(ref sc)) => sc,
        _ => return None,
    };

    if ssh_config.users.is_empty() {
        return None;
    }

    let mut data = BTreeMap::new();
    for user in &ssh_config.users {
        if !user.public_keys.is_empty() {
            data.insert(user.name.clone(), user.public_keys.clone());
        }
    }

    if data.is_empty() {
        return None;
    }

    Some(Secret {
        metadata: ObjectMeta {
            name: Some(common::child_name(app, "authorized-keys")),
            namespace: Some(common::app_namespace(app)),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        string_data: Some(data),
        type_: Some("Opaque".into()),
        ..Default::default()
    })
}
