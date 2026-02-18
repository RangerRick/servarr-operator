use serde::Deserialize;

use crate::client::{ApiError, HttpClient};
use crate::health::HealthCheck;

/// Client for the SABnzbd API.
///
/// SABnzbd uses a query-parameter-based API:
/// `GET /api?mode=<action>&apikey=<key>&output=json`
#[derive(Debug, Clone)]
pub struct SabnzbdClient {
    http: HttpClient,
    api_key: String,
}

// --- Response types ---

#[derive(Debug, Clone, Deserialize)]
pub struct VersionResponse {
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueueResponse {
    pub queue: QueueStatus,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStatus {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub speed: String,
    #[serde(default, rename = "sizeleft")]
    pub size_left: String,
    #[serde(default, rename = "mb")]
    pub total_mb: String,
    #[serde(default, rename = "mbleft")]
    pub mb_left: String,
    #[serde(default, rename = "noofslots_total")]
    pub total_slots: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerStatsResponse {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub servers: serde_json::Value,
}

impl SabnzbdClient {
    /// Create a new SABnzbd client.
    ///
    /// `base_url` should be the root URL (e.g. `http://sabnzbd:8080`).
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        let url = format!("{}/api", base_url.trim_end_matches('/'));
        Ok(Self {
            http: HttpClient::new(&url, None)?,
            api_key: api_key.to_string(),
        })
    }

    /// GET `/api?mode=version&apikey=<key>&output=json`
    pub async fn version(&self) -> Result<String, ApiError> {
        let resp: VersionResponse = self
            .http
            .get(&format!(
                "?mode=version&apikey={}&output=json",
                self.api_key
            ))
            .await?;
        Ok(resp.version)
    }

    /// GET `/api?mode=queue&apikey=<key>&output=json`
    pub async fn queue_status(&self) -> Result<QueueStatus, ApiError> {
        let resp: QueueResponse = self
            .http
            .get(&format!("?mode=queue&apikey={}&output=json", self.api_key))
            .await?;
        Ok(resp.queue)
    }

    /// GET `/api?mode=server_stats&apikey=<key>&output=json`
    pub async fn server_stats(&self) -> Result<ServerStatsResponse, ApiError> {
        self.http
            .get(&format!(
                "?mode=server_stats&apikey={}&output=json",
                self.api_key
            ))
            .await
    }
}

impl HealthCheck for SabnzbdClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let version = self.version().await?;
        Ok(!version.is_empty())
    }
}
