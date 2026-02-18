use serde::Deserialize;

use crate::client::{ApiError, HttpClient};
use crate::health::HealthCheck;

/// Client for the Servarr v3 REST API shared by Sonarr, Radarr, Lidarr, and Prowlarr.
///
/// All four applications expose identical endpoints under `/api/v3/` and
/// authenticate via the `X-Api-Key` header (handled by [`HttpClient`]).
#[derive(Debug, Clone)]
pub struct ServarrClient {
    http: HttpClient,
}

// --- Response types ---

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    pub app_name: String,
    pub version: String,
    #[serde(default)]
    pub build_time: String,
    #[serde(default)]
    pub is_debug: bool,
    #[serde(default)]
    pub is_production: bool,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub is_user_interactive: bool,
    #[serde(default)]
    pub startup_path: String,
    #[serde(default)]
    pub app_data: String,
    #[serde(default)]
    pub os_name: String,
    #[serde(default)]
    pub os_version: String,
    #[serde(default)]
    pub runtime_name: String,
    #[serde(default)]
    pub runtime_version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckResult {
    pub source: String,
    #[serde(rename = "type")]
    pub check_type: String,
    pub message: String,
    #[serde(default)]
    pub wiki_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootFolder {
    pub id: i64,
    pub path: String,
    #[serde(default)]
    pub accessible: bool,
    #[serde(default)]
    pub free_space: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub installable: bool,
    #[serde(default)]
    pub latest: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    pub id: i64,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub time: String,
}

impl ServarrClient {
    /// Create a new Servarr v3 API client.
    ///
    /// `base_url` should be the root URL (e.g. `http://sonarr:8989`).
    /// The `/api/v3/` prefix is appended automatically.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        let url = format!("{}/api/v3/", base_url.trim_end_matches('/'));
        Ok(Self {
            http: HttpClient::new(&url, Some(api_key))?,
        })
    }

    /// GET `/api/v3/system/status`
    pub async fn system_status(&self) -> Result<SystemStatus, ApiError> {
        self.http.get("system/status").await
    }

    /// GET `/api/v3/health`
    pub async fn health(&self) -> Result<Vec<HealthCheckResult>, ApiError> {
        self.http.get("health").await
    }

    /// GET `/api/v3/rootfolder`
    pub async fn root_folder(&self) -> Result<Vec<RootFolder>, ApiError> {
        self.http.get("rootfolder").await
    }

    /// GET `/api/v3/update` — returns available updates.
    pub async fn updates(&self) -> Result<Vec<UpdateInfo>, ApiError> {
        self.http.get("update").await
    }

    /// GET `/api/v3/system/backup` — list all backups.
    pub async fn list_backups(&self) -> Result<Vec<Backup>, ApiError> {
        self.http.get("system/backup").await
    }

    /// POST `/api/v3/system/backup` — create a new backup.
    pub async fn create_backup(&self) -> Result<Backup, ApiError> {
        self.http
            .post("system/backup", &serde_json::json!({}))
            .await
    }

    /// POST `/api/v3/system/backup/restore/{id}` — restore from a backup.
    pub async fn restore_backup(&self, id: i64) -> Result<(), ApiError> {
        let _: serde_json::Value = self
            .http
            .post(
                &format!("system/backup/restore/{id}"),
                &serde_json::json!({}),
            )
            .await?;
        Ok(())
    }

    /// DELETE `/api/v3/system/backup/{id}` — delete a backup.
    pub async fn delete_backup(&self, id: i64) -> Result<(), ApiError> {
        self.http.delete(&format!("system/backup/{id}")).await
    }
}

impl HealthCheck for ServarrClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let status = self.system_status().await?;
        Ok(!status.version.is_empty())
    }
}
