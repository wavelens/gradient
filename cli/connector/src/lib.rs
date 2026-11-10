// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

pub mod error;
pub use error::ConnectorError;

pub mod admin;
pub mod auth;
pub mod build_requests;
pub mod builds;
pub mod caches;
pub mod commits;
pub mod evals;
pub mod exec_jobs;
pub mod flake_inputs;
pub mod integrations;
pub mod projects;
pub mod pull_requests;
pub mod search;
pub mod server;
pub mod task_event_actions;
pub mod task_repository;
pub mod tasks;
pub mod teams;
pub mod user;
pub mod webhooks;
pub mod workers;

mod http;
pub use http::{DownloadedBinaryFile, LiveUpdateEvent};

pub mod api {
    include!(concat!(env!("OUT_DIR"), "/api.rs"));
}

use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    http: reqwest::Client,
    stream_http: reqwest::Client,
    timeout: Duration,
    base_url: String,
    token: Option<String>,
}

impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }
    pub(crate) fn stream_http(&self) -> &reqwest::Client {
        &self.inner.stream_http
    }
    pub(crate) fn transfer_no_progress_timer(&self) -> http::TransferNoProgressTimer {
        http::TransferNoProgressTimer::new(self.inner.timeout)
    }
    pub(crate) fn base_url(&self) -> &str {
        &self.inner.base_url
    }
    pub(crate) fn token(&self) -> Option<&str> {
        self.inner.token.as_deref()
    }

    pub fn admin(&self) -> admin::AdminApi<'_> {
        admin::AdminApi(self)
    }
    pub fn auth(&self) -> auth::AuthApi<'_> {
        auth::AuthApi(self)
    }
    pub fn build_requests(&self) -> build_requests::BuildRequestsApi<'_> {
        build_requests::BuildRequestsApi(self)
    }
    pub fn builds(&self) -> builds::BuildsApi<'_> {
        builds::BuildsApi(self)
    }
    pub fn caches(&self) -> caches::CachesApi<'_> {
        caches::CachesApi(self)
    }
    pub fn commits(&self) -> commits::CommitsApi<'_> {
        commits::CommitsApi(self)
    }
    pub fn evals(&self) -> evals::EvalsApi<'_> {
        evals::EvalsApi(self)
    }
    pub fn exec_jobs(&self) -> exec_jobs::ExecJobsApi<'_> {
        exec_jobs::ExecJobsApi(self)
    }
    pub fn integrations(&self) -> integrations::IntegrationsApi<'_> {
        integrations::IntegrationsApi(self)
    }
    pub fn projects(&self) -> projects::ProjectsApi<'_> {
        projects::ProjectsApi(self)
    }
    pub fn pull_requests(&self) -> pull_requests::PullRequestsApi<'_> {
        pull_requests::PullRequestsApi(self)
    }
    pub fn tasks(&self) -> tasks::TasksApi<'_> {
        tasks::TasksApi(self)
    }
    pub fn task_event_actions(&self) -> task_event_actions::TaskActionsApi<'_> {
        task_event_actions::TaskActionsApi(self)
    }
    pub fn flake_inputs(&self) -> flake_inputs::TaskFlakeInputOverridesApi<'_> {
        flake_inputs::TaskFlakeInputOverridesApi(self)
    }
    pub fn task_repository(&self) -> task_repository::TaskRepositoryConnectionApi<'_> {
        task_repository::TaskRepositoryConnectionApi(self)
    }
    pub fn search(&self) -> search::SearchApi<'_> {
        search::SearchApi(self)
    }
    pub fn server(&self) -> server::ServerApi<'_> {
        server::ServerApi(self)
    }
    pub fn teams(&self) -> teams::TeamsApi<'_> {
        teams::TeamsApi(self)
    }
    pub fn user(&self) -> user::UserApi<'_> {
        user::UserApi(self)
    }
    pub fn webhooks(&self) -> webhooks::WebhooksApi<'_> {
        webhooks::WebhooksApi(self)
    }
    pub fn workers(&self) -> workers::WorkersApi<'_> {
        workers::WorkersApi(self)
    }

    pub(crate) async fn send_authenticated_request<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        endpoint: &str,
        body: Option<&(impl serde::Serialize + ?Sized)>,
    ) -> Result<T, ConnectorError> {
        let mut req = http::request(
            self.http(),
            self.base_url(),
            self.token(),
            method,
            endpoint,
            true,
        )?;
        if let Some(body) = body {
            req = req.json(body);
        }
        http::decode(req.send().await?).await
    }

    pub async fn health(&self) -> Result<String, ConnectorError> {
        let req = http::request(
            self.http(),
            self.base_url(),
            self.token(),
            reqwest::Method::GET,
            "health",
            false,
        )?;
        http::decode(req.send().await?).await
    }
}

#[derive(Default)]
pub struct ClientBuilder {
    base_url: Option<String>,
    token: Option<String>,
    timeout: Option<Duration>,
}

impl ClientBuilder {
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    pub fn build(self) -> Result<Client, String> {
        let base_url = self
            .base_url
            .ok_or_else(|| "base_url is required".to_string())?;
        let timeout = self.timeout.unwrap_or(Duration::from_secs(30));
        let http = http_client(reqwest::Client::builder().timeout(timeout))?;
        // A total timeout would also bound the body and cut off long-lived log streams.
        let stream_http = http_client(reqwest::Client::builder().connect_timeout(timeout))?;
        Ok(Client {
            inner: Arc::new(ClientInner {
                http,
                stream_http,
                timeout,
                base_url,
                token: self.token,
            }),
        })
    }
}

fn http_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client, String> {
    builder
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("gradient-cli/", env!("CARGO_PKG_VERSION")))
        .use_preconfigured_tls(rustls_config())
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))
}

fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn rustls_root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    roots
}

pub(crate) fn rustls_config() -> rustls::ClientConfig {
    init_crypto_provider();
    rustls::ClientConfig::builder()
        .with_root_certificates(rustls_root_store())
        .with_no_client_auth()
}
