use crate::client::ApiError;

/// Client for the Overseerr settings API.
///
/// Wraps the `overseerr` crate to manage Sonarr/Radarr server registrations
/// in Overseerr for media request routing.
pub struct OverseerrClient {
    config: overseerr::apis::configuration::Configuration,
}

fn map_err<E: std::fmt::Debug>(e: overseerr::apis::Error<E>) -> ApiError {
    ApiError::ApiResponse {
        status: 0,
        body: format!("{e:?}"),
    }
}

impl OverseerrClient {
    /// Create a new Overseerr API client.
    ///
    /// `base_url` should be the root URL (e.g. `http://overseerr:5055`).
    /// `api_key` is sent as the `X-Api-Key` header.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        let mut config = overseerr::apis::configuration::Configuration::new();
        config.base_path = base_url.trim_end_matches('/').to_string();
        config.api_key = Some(overseerr::apis::configuration::ApiKey {
            prefix: None,
            key: api_key.to_string(),
        });
        Self { config }
    }

    /// List all Sonarr server registrations.
    pub async fn list_sonarr(&self) -> Result<Vec<overseerr::models::SonarrSettings>, ApiError> {
        overseerr::apis::settings_api::list_sonarr(&self.config)
            .await
            .map_err(map_err)
    }

    /// Register a new Sonarr server.
    pub async fn create_sonarr(
        &self,
        settings: overseerr::models::SonarrSettings,
    ) -> Result<overseerr::models::SonarrSettings, ApiError> {
        overseerr::apis::settings_api::create_sonarr(&self.config, settings)
            .await
            .map_err(map_err)
    }

    /// Update an existing Sonarr server registration.
    pub async fn update_sonarr(
        &self,
        id: i32,
        settings: overseerr::models::SonarrSettings,
    ) -> Result<overseerr::models::SonarrSettings, ApiError> {
        overseerr::apis::settings_api::update_sonarr(&self.config, id, settings)
            .await
            .map_err(map_err)
    }

    /// Remove a Sonarr server registration.
    pub async fn delete_sonarr(&self, id: i32) -> Result<(), ApiError> {
        overseerr::apis::settings_api::delete_sonarr(&self.config, id)
            .await
            .map_err(map_err)
            .map(|_| ())
    }

    /// List all Radarr server registrations.
    pub async fn list_radarr(&self) -> Result<Vec<overseerr::models::RadarrSettings>, ApiError> {
        overseerr::apis::settings_api::list_radarr(&self.config)
            .await
            .map_err(map_err)
    }

    /// Register a new Radarr server.
    pub async fn create_radarr(
        &self,
        settings: overseerr::models::RadarrSettings,
    ) -> Result<overseerr::models::RadarrSettings, ApiError> {
        overseerr::apis::settings_api::create_radarr(&self.config, settings)
            .await
            .map_err(map_err)
    }

    /// Update an existing Radarr server registration.
    pub async fn update_radarr(
        &self,
        id: i32,
        settings: overseerr::models::RadarrSettings,
    ) -> Result<overseerr::models::RadarrSettings, ApiError> {
        overseerr::apis::settings_api::update_radarr(&self.config, id, settings)
            .await
            .map_err(map_err)
    }

    /// Remove a Radarr server registration.
    pub async fn delete_radarr(&self, id: i32) -> Result<(), ApiError> {
        overseerr::apis::settings_api::delete_radarr(&self.config, id)
            .await
            .map_err(map_err)
            .map(|_| ())
    }
}
