pub mod error;
pub use error::ConnectorError;

pub mod admin;
pub mod auth;
pub mod build_requests;
pub mod builds;
pub mod caches;
pub mod commits;
pub mod evals;
pub mod integrations;
pub mod orgs;
pub mod projects;
pub mod server;
pub mod user;
pub mod webhooks;
pub mod workers;

mod http;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    http: reqwest::Client,
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
    pub fn integrations(&self) -> integrations::IntegrationsApi<'_> {
        integrations::IntegrationsApi(self)
    }
    pub fn orgs(&self) -> orgs::OrgsApi<'_> {
        orgs::OrgsApi(self)
    }
    pub fn projects(&self) -> projects::ProjectsApi<'_> {
        projects::ProjectsApi(self)
    }
    pub fn server(&self) -> server::ServerApi<'_> {
        server::ServerApi(self)
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
        let http = reqwest::Client::builder()
            .timeout(self.timeout.unwrap_or(Duration::from_secs(30)))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("gradient-cli/", env!("CARGO_PKG_VERSION")))
            .use_preconfigured_tls(rustls_config())
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Client {
            inner: Arc::new(ClientInner {
                http,
                base_url,
                token: self.token,
            }),
        })
    }
}

fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn rustls_root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

fn rustls_config() -> rustls::ClientConfig {
    init_crypto_provider();
    rustls::ClientConfig::builder()
        .with_root_certificates(rustls_root_store())
        .with_no_client_auth()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression for #287: native certs must be merged into the root store
    // (alongside webpki-roots) so self-signed CAs installed in the OS trust
    // store are honoured.
    #[test]
    fn root_store_contains_webpki_baseline() {
        let roots = rustls_root_store();
        assert!(
            roots.len() >= webpki_roots::TLS_SERVER_ROOTS.len(),
            "root store missing webpki baseline",
        );
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ListItem {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Paginated<T> {
    pub items: T,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

pub type ListResponse = Vec<ListItem>;
pub type PaginatedListResponse = Paginated<Vec<ListItem>>;
