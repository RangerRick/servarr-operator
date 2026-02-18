use crate::client::{ApiError, HttpClient};
use crate::health::HealthCheck;

/// Client for the Jellyfin API.
///
/// Jellyfin exposes a `GET /health` endpoint that returns `"Healthy"`
/// (text/plain, HTTP 200) when the server is running. No API key required.
#[derive(Debug, Clone)]
pub struct JellyfinClient {
    http: HttpClient,
}

impl JellyfinClient {
    pub fn new(base_url: &str) -> Result<Self, ApiError> {
        Ok(Self {
            http: HttpClient::new(base_url, None)?,
        })
    }
}

impl HealthCheck for JellyfinClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let url = self.http.base_url().join("/health")?;
        let resp = self.http.inner().get(url).send().await?;
        Ok(resp.status().is_success())
    }
}
