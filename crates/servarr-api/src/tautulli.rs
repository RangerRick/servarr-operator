use crate::client::{ApiError, HttpClient};
use crate::health::HealthCheck;

/// Client for the Tautulli API.
///
/// Tautulli uses a query-parameter-based API:
/// `GET /api/v2?cmd=<action>&...`
#[derive(Debug, Clone)]
pub struct TautulliClient {
    http: HttpClient,
}

impl TautulliClient {
    /// Create a new Tautulli client.
    ///
    /// `base_url` should be the root URL (e.g. `http://tautulli:8181`).
    pub fn new(base_url: &str) -> Result<Self, ApiError> {
        let url = format!("{}/api/v2", base_url.trim_end_matches('/'));
        Ok(Self {
            http: HttpClient::new(&url, None)?,
        })
    }

    /// Set admin credentials via the `set_credentials` command.
    ///
    /// Calls `GET /api/v2?cmd=set_credentials&username=...&password=...`.
    pub async fn set_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), ApiError> {
        let mut url = self.http.base_url().clone();
        url.query_pairs_mut()
            .append_pair("cmd", "set_credentials")
            .append_pair("username", username)
            .append_pair("password", password);

        let resp = self
            .http
            .inner()
            .get(url)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }
}

impl HealthCheck for TautulliClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let mut url = self.http.base_url().clone();
        url.query_pairs_mut()
            .append_pair("cmd", "status")
            .append_pair("output", "json");

        let resp = self
            .http
            .inner()
            .get(url)
            .send()
            .await
            .map_err(ApiError::Request)?;

        if !resp.status().is_success() {
            return Ok(false);
        }
        let body: serde_json::Value = resp.json().await.map_err(ApiError::Request)?;
        // Tautulli returns {"response": {"result": "success", ...}}
        Ok(body
            .get("response")
            .and_then(|r| r.get("result"))
            .and_then(|r| r.as_str())
            == Some("success"))
    }
}
