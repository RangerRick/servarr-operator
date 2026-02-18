use serde::{Deserialize, Serialize};

use crate::client::ApiError;

fn map_sdk_err<E: std::fmt::Debug>(e: E) -> ApiError {
    ApiError::ApiResponse {
        status: 0,
        body: format!("{e:?}"),
    }
}

/// Client for the Prowlarr v1 application management API.
///
/// Prowlarr manages indexer proxies ("applications") that sync indexers to
/// downstream *arr apps (Sonarr, Radarr, Lidarr). This client wraps the
/// prowlarr SDK crate.
#[derive(Debug, Clone)]
pub struct ProwlarrClient {
    config: prowlarr::apis::configuration::Configuration,
}

/// An application registration in Prowlarr.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrApp {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub sync_level: String,
    #[serde(default)]
    pub implementation: String,
    #[serde(default)]
    pub config_contract: String,
    #[serde(default)]
    pub fields: Vec<ProwlarrAppField>,
    #[serde(default)]
    pub tags: Vec<i64>,
}

/// A field in a Prowlarr application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrAppField {
    pub name: String,
    #[serde(default)]
    pub value: serde_json::Value,
}

// --- Conversion helpers between our types and SDK types ---

fn sdk_to_app(r: prowlarr::models::ApplicationResource) -> ProwlarrApp {
    let fields = r
        .fields
        .and_then(|outer| outer)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| {
            let name = f.name?.unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            let value = f.value.and_then(|v| v).unwrap_or(serde_json::Value::Null);
            Some(ProwlarrAppField { name, value })
        })
        .collect();

    let sync_level = r.sync_level.map(|s| s.to_string()).unwrap_or_default();

    let tags = r
        .tags
        .and_then(|outer| outer)
        .unwrap_or_default()
        .into_iter()
        .map(|t| t as i64)
        .collect();

    ProwlarrApp {
        id: r.id.unwrap_or(0) as i64,
        name: r.name.and_then(|n| n).unwrap_or_default(),
        sync_level,
        implementation: r.implementation.and_then(|i| i).unwrap_or_default(),
        config_contract: r.config_contract.and_then(|c| c).unwrap_or_default(),
        fields,
        tags,
    }
}

fn app_to_sdk(app: &ProwlarrApp) -> prowlarr::models::ApplicationResource {
    let fields: Vec<prowlarr::models::Field> = app
        .fields
        .iter()
        .map(|f| {
            let mut field = prowlarr::models::Field::new();
            field.name = Some(Some(f.name.clone()));
            field.value = Some(Some(f.value.clone()));
            field
        })
        .collect();

    let sync_level = match app.sync_level.as_str() {
        "disabled" => Some(prowlarr::models::ApplicationSyncLevel::Disabled),
        "addOnly" => Some(prowlarr::models::ApplicationSyncLevel::AddOnly),
        "fullSync" => Some(prowlarr::models::ApplicationSyncLevel::FullSync),
        _ => Some(prowlarr::models::ApplicationSyncLevel::FullSync),
    };

    let tags: Vec<i32> = app.tags.iter().map(|&t| t as i32).collect();

    let mut resource = prowlarr::models::ApplicationResource::new();
    resource.id = if app.id != 0 {
        Some(app.id as i32)
    } else {
        None
    };
    resource.name = Some(Some(app.name.clone()));
    resource.sync_level = sync_level;
    resource.implementation = Some(Some(app.implementation.clone()));
    resource.config_contract = Some(Some(app.config_contract.clone()));
    resource.fields = Some(Some(fields));
    resource.tags = Some(Some(tags));
    resource
}

impl ProwlarrClient {
    /// Create a new Prowlarr API client.
    ///
    /// `base_url` should be the root URL (e.g. `http://prowlarr:9696`).
    /// `api_key` is sent as the `X-Api-Key` header.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        let mut config = prowlarr::apis::configuration::Configuration::new();
        config.base_path = base_url.trim_end_matches('/').to_string();
        config.api_key = Some(prowlarr::apis::configuration::ApiKey {
            prefix: None,
            key: api_key.to_string(),
        });
        Ok(Self { config })
    }

    /// GET `/api/v1/applications` — list all registered applications.
    pub async fn list_applications(&self) -> Result<Vec<ProwlarrApp>, ApiError> {
        prowlarr::apis::application_api::list_applications(&self.config)
            .await
            .map(|v| v.into_iter().map(sdk_to_app).collect())
            .map_err(map_sdk_err)
    }

    /// POST `/api/v1/applications` — add a new application.
    pub async fn add_application(&self, app: &ProwlarrApp) -> Result<ProwlarrApp, ApiError> {
        let resource = app_to_sdk(app);
        prowlarr::apis::application_api::create_applications(&self.config, None, Some(resource))
            .await
            .map(sdk_to_app)
            .map_err(map_sdk_err)
    }

    /// PUT `/api/v1/applications/{id}` — update an existing application.
    pub async fn update_application(
        &self,
        id: i64,
        app: &ProwlarrApp,
    ) -> Result<ProwlarrApp, ApiError> {
        let resource = app_to_sdk(app);
        prowlarr::apis::application_api::update_applications(
            &self.config,
            &id.to_string(),
            None,
            Some(resource),
        )
        .await
        .map(sdk_to_app)
        .map_err(map_sdk_err)
    }

    /// DELETE `/api/v1/applications/{id}` — remove an application.
    pub async fn delete_application(&self, id: i64) -> Result<(), ApiError> {
        prowlarr::apis::application_api::delete_applications(&self.config, id as i32)
            .await
            .map_err(map_sdk_err)
    }
}
