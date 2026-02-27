use serde::{Deserialize, Serialize};

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

/// Jellyfin user record (minimal fields needed for credential management).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JellyfinUser {
    pub id: String,
    pub name: String,
}

/// Jellyfin auth response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AuthResponse {
    access_token: String,
}

/// Jellyfin set-password request body.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct SetPasswordRequest<'a> {
    current_pw: &'a str,
    new_pw: &'a str,
    reset_password: bool,
}

/// Jellyfin authenticate request body.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct AuthenticateRequest<'a> {
    username: &'a str,
    pw: &'a str,
}

/// Jellyfin startup user request body.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StartupUserRequest<'a> {
    name: &'a str,
    password: &'a str,
}

/// Authorization header value required by the Jellyfin startup wizard endpoints.
const JELLYFIN_AUTH_HEADER: &str = concat!(
    r#"MediaBrowser Client="servarr-operator", "#,
    r#"Device="servarr-operator", "#,
    r#"DeviceId="servarr-operator-device", "#,
    r#"Version="1.0.0""#
);

impl JellyfinClient {
    pub fn new(base_url: &str) -> Result<Self, ApiError> {
        Ok(Self {
            http: HttpClient::new(base_url, None)?,
        })
    }

    /// Return `true` if the startup wizard has not been completed yet.
    ///
    /// Calls `GET /Startup/Configuration`; a 200 response means the wizard is pending.
    pub async fn startup_pending(&self) -> Result<bool, ApiError> {
        let url = self.http.base_url().join("/Startup/Configuration")?;
        let resp = self
            .http
            .inner()
            .get(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
            .send()
            .await
            .map_err(ApiError::Request)?;
        Ok(resp.status().as_u16() == 200)
    }

    /// Set the initial admin user via the startup wizard (`POST /Startup/User`).
    pub async fn startup_set_user(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), ApiError> {
        let url = self.http.base_url().join("/Startup/User")?;
        let body = StartupUserRequest { name: username, password };
        let resp = self
            .http
            .inner()
            .post(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
            .json(&body)
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

    /// Authenticate as a user and return the access token.
    pub async fn authenticate(
        &self,
        username: &str,
        password: &str,
    ) -> Result<String, ApiError> {
        let url = self.http.base_url().join("/Users/AuthenticateByName")?;
        let body = AuthenticateRequest { username, pw: password };
        let resp = self
            .http
            .inner()
            .post(url)
            .header("X-Emby-Authorization", JELLYFIN_AUTH_HEADER)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Request)?;
        if resp.status().is_success() {
            let auth: AuthResponse = resp.json().await.map_err(ApiError::Request)?;
            Ok(auth.access_token)
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// List all users (requires an authenticated token).
    pub async fn list_users(&self, token: &str) -> Result<Vec<JellyfinUser>, ApiError> {
        let url = self.http.base_url().join("/Users")?;
        let resp = self
            .http
            .inner()
            .get(url)
            .header(
                "X-Emby-Authorization",
                format!("{JELLYFIN_AUTH_HEADER}, Token=\"{token}\""),
            )
            .send()
            .await
            .map_err(ApiError::Request)?;
        if resp.status().is_success() {
            resp.json::<Vec<JellyfinUser>>()
                .await
                .map_err(ApiError::Request)
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::ApiResponse { status, body })
        }
    }

    /// Change a user's password (`POST /Users/{userId}/Password`).
    pub async fn set_password(
        &self,
        token: &str,
        user_id: &str,
        new_password: &str,
    ) -> Result<(), ApiError> {
        let url = self
            .http
            .base_url()
            .join(&format!("/Users/{user_id}/Password"))?;
        let body = SetPasswordRequest {
            current_pw: "",
            new_pw: new_password,
            reset_password: true,
        };
        let resp = self
            .http
            .inner()
            .post(url)
            .header(
                "X-Emby-Authorization",
                format!("{JELLYFIN_AUTH_HEADER}, Token=\"{token}\""),
            )
            .json(&body)
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

    /// Configure the Jellyfin admin account.
    ///
    /// If the startup wizard is pending, sets the initial admin via the wizard.
    /// Otherwise, authenticates as the existing admin and changes the password.
    pub async fn configure_admin(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), ApiError> {
        if self.startup_pending().await? {
            return self.startup_set_user(username, password).await;
        }

        // Wizard complete: authenticate as the admin user and update the password.
        let token = self.authenticate(username, password).await?;
        let users = self.list_users(&token).await?;
        let admin = users
            .into_iter()
            .find(|u| u.name.eq_ignore_ascii_case(username))
            .ok_or_else(|| ApiError::ApiResponse {
                status: 404,
                body: format!("user '{username}' not found in Jellyfin"),
            })?;
        self.set_password(&token, &admin.id, password).await
    }
}

impl HealthCheck for JellyfinClient {
    async fn is_healthy(&self) -> Result<bool, ApiError> {
        let url = self.http.base_url().join("/health")?;
        let resp = self.http.inner().get(url).send().await?;
        Ok(resp.status().is_success())
    }
}
