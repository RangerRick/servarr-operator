use servarr_api::{
    ApiError, HttpClient, JellyfinClient, PlexClient, SabnzbdClient, TransmissionClient,
};
use servarr_api::HealthCheck;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// HttpClient tests
// ---------------------------------------------------------------------------

mod http_client {
    use super::*;

    #[test]
    fn new_with_valid_url() {
        let client = HttpClient::new("http://localhost:8080", Some("test-key"));
        assert!(client.is_ok());
    }

    #[test]
    fn new_with_invalid_url() {
        let result = HttpClient::new("not a url", None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ApiError::InvalidUrl(_)));
    }

    #[test]
    fn new_with_non_ascii_api_key() {
        let result = HttpClient::new("http://localhost:8080", Some("key\x01bad"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidApiKey),
            "expected InvalidApiKey, got: {err}"
        );
    }

    #[test]
    fn base_url_returns_parsed_url() {
        let client = HttpClient::new("http://example.com:9090/", None).unwrap();
        assert_eq!(client.base_url().as_str(), "http://example.com:9090/");
    }

    #[test]
    fn debug_impl_shows_base_url() {
        let client = HttpClient::new("http://example.com:9090/", None).unwrap();
        let debug = format!("{client:?}");
        assert!(
            debug.contains("http://example.com:9090/"),
            "Debug output should contain base_url, got: {debug}"
        );
        assert!(
            debug.contains("HttpClient"),
            "Debug output should contain struct name, got: {debug}"
        );
    }

    #[tokio::test]
    async fn get_returns_json_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/system/status"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"version": "4.0.0"})),
            )
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), Some("test-key")).unwrap();
        let resp: serde_json::Value = client.get("api/v3/system/status").await.unwrap();
        assert_eq!(resp["version"], "4.0.0");
    }

    #[tokio::test]
    async fn get_returns_api_error_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/system/status"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let result: Result<serde_json::Value, _> = client.get("api/v3/system/status").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ApiError::ApiResponse { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, "internal error");
            }
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }

    #[tokio::test]
    async fn post_sends_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/command"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 1, "name": "RefreshSeries"})),
            )
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let body = serde_json::json!({"name": "RefreshSeries"});
        let resp: serde_json::Value = client.post("api/v3/command", &body).await.unwrap();
        assert_eq!(resp["name"], "RefreshSeries");
    }

    #[tokio::test]
    async fn put_sends_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v3/series/1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 1, "title": "Updated"})),
            )
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let body = serde_json::json!({"id": 1, "title": "Updated"});
        let resp: serde_json::Value = client.put("api/v3/series/1", &body).await.unwrap();
        assert_eq!(resp["title"], "Updated");
    }

    #[tokio::test]
    async fn delete_succeeds_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v3/series/1"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let result = client.delete("api/v3/series/1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn delete_returns_error_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v3/series/999"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let client = HttpClient::new(&server.uri(), None).unwrap();
        let result = client.delete("api/v3/series/999").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ApiError::ApiResponse { status, body } => {
                assert_eq!(status, 404);
                assert_eq!(body, "not found");
            }
            other => panic!("expected ApiResponse, got: {other}"),
        }
    }
}

// ---------------------------------------------------------------------------
// TransmissionClient tests
// ---------------------------------------------------------------------------

mod transmission_client {
    use super::*;

    #[test]
    fn new_constructs_without_credentials() {
        let client = TransmissionClient::new("http://localhost:9091", None, None);
        assert!(client.is_ok());
    }

    #[test]
    fn new_constructs_with_credentials() {
        let client =
            TransmissionClient::new("http://localhost:9091", Some("admin"), Some("secret"));
        assert!(client.is_ok());
    }

    #[test]
    fn new_rejects_invalid_url() {
        let result = TransmissionClient::new("not a url", None, None);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn session_get_with_409_handshake() {
        let server = MockServer::start().await;
        let session_id = "test-session-id-12345";

        // First request returns 409 with session ID header
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(409)
                    .append_header("X-Transmission-Session-Id", session_id),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        // Second request (with session ID) returns 200 with session info
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "version": "4.0.5",
                    "rpc-version": 18,
                    "rpc-version-minimum": 14,
                    "download-dir": "/downloads",
                    "config-dir": "/config"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        let info = client.session_get().await.unwrap();
        assert_eq!(info.version, "4.0.5");
        assert_eq!(info.rpc_version, 18);
        assert_eq!(info.download_dir, "/downloads");
    }

    #[tokio::test]
    async fn session_stats_returns_stats() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "activeTorrentCount": 3,
                    "pausedTorrentCount": 1,
                    "torrentCount": 4,
                    "downloadSpeed": 1048576,
                    "uploadSpeed": 524288
                }
            })))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        let stats = client.session_stats().await.unwrap();
        assert_eq!(stats.active_torrent_count, 3);
        assert_eq!(stats.paused_torrent_count, 1);
        assert_eq!(stats.torrent_count, 4);
        assert_eq!(stats.download_speed, 1_048_576);
        assert_eq!(stats.upload_speed, 524_288);
    }

    #[tokio::test]
    async fn health_check_healthy_when_version_present() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "version": "4.0.5"
                }
            })))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        assert!(client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn health_check_unhealthy_when_version_empty() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "arguments": {
                    "version": ""
                }
            })))
            .mount(&server)
            .await;

        let client = TransmissionClient::new(&server.uri(), None, None).unwrap();
        assert!(!client.is_healthy().await.unwrap());
    }
}

// ---------------------------------------------------------------------------
// SabnzbdClient tests
// ---------------------------------------------------------------------------

mod sabnzbd_client {
    use super::*;

    #[test]
    fn new_constructs_client() {
        let client = SabnzbdClient::new("http://localhost:8080", "my-api-key");
        assert!(client.is_ok());
    }

    #[test]
    fn new_rejects_invalid_url() {
        let result = SabnzbdClient::new("not a url", "key");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn version_returns_version_string() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"version": "4.2.1"})),
            )
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        let version = client.version().await.unwrap();
        assert_eq!(version, "4.2.1");
    }

    #[tokio::test]
    async fn queue_status_returns_queue() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "queue": {
                    "status": "Downloading",
                    "speed": "10.5 M",
                    "sizeleft": "1.2 GB",
                    "mb": "5000.00",
                    "mbleft": "1200.00",
                    "noofslots_total": "5"
                }
            })))
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        let queue = client.queue_status().await.unwrap();
        assert_eq!(queue.status, "Downloading");
        assert_eq!(queue.speed, "10.5 M");
        assert_eq!(queue.size_left, "1.2 GB");
        assert_eq!(queue.total_mb, "5000.00");
        assert_eq!(queue.mb_left, "1200.00");
        assert_eq!(queue.total_slots, "5");
    }

    #[tokio::test]
    async fn server_stats_returns_stats() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 1024000,
                "servers": {
                    "news.example.com": {
                        "total": 1024000
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        let stats = client.server_stats().await.unwrap();
        assert_eq!(stats.total, 1_024_000);
        assert!(stats.servers.is_object());
    }

    #[tokio::test]
    async fn health_check_healthy() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"version": "4.2.1"})),
            )
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        assert!(client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn health_check_unhealthy_when_version_empty() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"version": ""})),
            )
            .mount(&server)
            .await;

        let client = SabnzbdClient::new(&server.uri(), "test-key").unwrap();
        assert!(!client.is_healthy().await.unwrap());
    }
}

// ---------------------------------------------------------------------------
// PlexClient tests
// ---------------------------------------------------------------------------

mod plex_client {
    use super::*;

    #[test]
    fn new_constructs_client() {
        let client = PlexClient::new("http://localhost:32400");
        assert!(client.is_ok());
    }

    #[test]
    fn new_rejects_invalid_url() {
        let result = PlexClient::new("not a url");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn is_healthy_returns_true_on_200() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/identity"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<MediaContainer size=\"0\"/>"),
            )
            .mount(&server)
            .await;

        let client = PlexClient::new(&server.uri()).unwrap();
        assert!(client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn is_healthy_returns_false_on_500() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/identity"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = PlexClient::new(&server.uri()).unwrap();
        assert!(!client.is_healthy().await.unwrap());
    }
}

// ---------------------------------------------------------------------------
// JellyfinClient tests
// ---------------------------------------------------------------------------

mod jellyfin_client {
    use super::*;

    #[test]
    fn new_constructs_client() {
        let client = JellyfinClient::new("http://localhost:8096");
        assert!(client.is_ok());
    }

    #[test]
    fn new_rejects_invalid_url() {
        let result = JellyfinClient::new("not a url");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn is_healthy_returns_true_on_200() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("Healthy"),
            )
            .mount(&server)
            .await;

        let client = JellyfinClient::new(&server.uri()).unwrap();
        assert!(client.is_healthy().await.unwrap());
    }

    #[tokio::test]
    async fn is_healthy_returns_false_on_500() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = JellyfinClient::new(&server.uri()).unwrap();
        assert!(!client.is_healthy().await.unwrap());
    }
}
