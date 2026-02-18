use k8s_openapi::api::core::v1::{
    PersistentVolumeClaim, PersistentVolumeClaimSpec, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use servarr_crds::{AppDefaults, PvcVolume, ServarrApp};
use std::collections::BTreeMap;

use crate::common;

pub fn build_all(app: &ServarrApp) -> Vec<PersistentVolumeClaim> {
    let defaults = AppDefaults::for_app(&app.spec.app);
    let persistence = app
        .spec
        .persistence
        .as_ref()
        .unwrap_or(&defaults.persistence);

    persistence
        .volumes
        .iter()
        .map(|v| build_one(app, v))
        .collect()
}

fn build_one(app: &ServarrApp, vol: &PvcVolume) -> PersistentVolumeClaim {
    let storage_class = if vol.storage_class.is_empty() {
        None
    } else {
        Some(vol.storage_class.clone())
    };

    PersistentVolumeClaim {
        metadata: common::metadata(app, &vol.name),
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec![vol.access_mode.clone()]),
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".into(),
                    Quantity(vol.size.clone()),
                )])),
                ..Default::default()
            }),
            storage_class_name: storage_class,
            ..Default::default()
        }),
        ..Default::default()
    }
}
