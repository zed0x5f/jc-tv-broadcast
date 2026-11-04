//! Async client for the daemon's localhost control API.
//!
//! Testing: no automated tests (the self-test uses curl, not this client).

use std::time::Duration;

use crate::api::*;
use crate::{Config, paths};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("daemon not reachable at {0} (is castdeskd running? `systemctl --user start castdeskd`)")]
    Unreachable(String),
    #[error("castdeskd is not running (no API token at {path}: {source}) — press s to start it, or `systemctl --user start castdeskd`")]
    Token {
        path: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Api(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Clone)]
pub struct ApiClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl ApiClient {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .expect("reqwest client");
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http,
        }
    }

    /// Build from the user's config file and the daemon's token file.
    pub fn from_environment() -> Result<Self, ClientError> {
        let cfg = Config::load(&paths::config_file()).unwrap_or_default();
        let path = paths::token_file();
        let token = std::fs::read_to_string(&path).map_err(|source| ClientError::Token {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Self::new(
            format!("http://{}", cfg.daemon.listen),
            token.trim().to_string(),
        ))
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, p: &str) -> String {
        format!("{}{}{}", self.base, API_PREFIX, p)
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T, ClientError> {
        let resp = req.bearer_auth(&self.token).send().await.map_err(|e| {
            if e.is_connect() {
                ClientError::Unreachable(self.base.clone())
            } else {
                ClientError::Http(e)
            }
        })?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp.json().await?)
        } else {
            let msg = match resp.json::<ApiError>().await {
                Ok(e) => e.error,
                Err(_) => status.to_string(),
            };
            Err(ClientError::Api(msg))
        }
    }

    pub async fn system(&self) -> Result<SystemInfo, ClientError> {
        self.send(self.http.get(self.url("/system"))).await
    }

    pub async fn displays(&self) -> Result<Vec<DisplayStatus>, ClientError> {
        self.send(self.http.get(self.url("/displays"))).await
    }

    pub async fn add_display(&self, spec: &DisplaySpec) -> Result<DisplayStatus, ClientError> {
        self.send(self.http.post(self.url("/displays")).json(spec)).await
    }

    pub async fn remove_display(&self, name: &str) -> Result<serde_json::Value, ClientError> {
        self.send(self.http.delete(self.url(&format!("/displays/{name}"))))
            .await
    }

    pub async fn restart_display(&self, name: &str) -> Result<serde_json::Value, ClientError> {
        self.send(self.http.post(self.url(&format!("/displays/{name}/restart"))))
            .await
    }
}
