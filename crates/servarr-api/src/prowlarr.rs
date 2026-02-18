use serde::{Deserialize, Serialize};

use crate::client::{ApiError, HttpClient};

/// Client for the Prowlarr v1 application management API.
///
/// Prowlarr manages indexer proxies ("applications") that sync indexers to
/// downstream *arr apps (Sonarr, Radarr, Lidarr). This client wraps the
/// `/api/v1/applications` endpoints.
#[derive(Debug, Clone)]
pub struct ProwlarrClient {
    http: HttpClient,
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

impl ProwlarrClient {
    /// Create a new Prowlarr API client.
    ///
    /// `base_url` should be the root URL (e.g. `http://prowlarr:9696`).
    /// The `/api/v1/` prefix is appended automatically.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        let url = format!("{}/api/v1/", base_url.trim_end_matches('/'));
        Ok(Self {
            http: HttpClient::new(&url, Some(api_key))?,
        })
    }

    /// GET `/api/v1/applications` — list all registered applications.
    pub async fn list_applications(&self) -> Result<Vec<ProwlarrApp>, ApiError> {
        self.http.get("applications").await
    }

    /// POST `/api/v1/applications` — add a new application.
    pub async fn add_application(&self, app: &ProwlarrApp) -> Result<ProwlarrApp, ApiError> {
        self.http.post("applications", app).await
    }

    /// PUT `/api/v1/applications/{id}` — update an existing application.
    pub async fn update_application(
        &self,
        id: i64,
        app: &ProwlarrApp,
    ) -> Result<ProwlarrApp, ApiError> {
        self.http.put(&format!("applications/{id}"), app).await
    }

    /// DELETE `/api/v1/applications/{id}` — remove an application.
    pub async fn delete_application(&self, id: i64) -> Result<(), ApiError> {
        self.http.delete(&format!("applications/{id}")).await
    }
}
