pub mod error;
pub use error::ConnectorError;

pub mod auth;
pub mod builds;
pub mod evals;
pub mod orgs;
pub mod projects;
pub mod server;
pub mod user;

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
    pub fn builder() -> ClientBuilder { ClientBuilder::default() }

    pub(crate) fn http(&self) -> &reqwest::Client { &self.inner.http }
    pub(crate) fn base_url(&self) -> &str { &self.inner.base_url }
    pub(crate) fn token(&self) -> Option<&str> { self.inner.token.as_deref() }

    pub fn auth(&self) -> auth::AuthApi<'_> { auth::AuthApi(self) }
    pub fn builds(&self) -> builds::BuildsApi<'_> { builds::BuildsApi(self) }
    pub fn evals(&self) -> evals::EvalsApi<'_> { evals::EvalsApi(self) }
    pub fn orgs(&self) -> orgs::OrgsApi<'_> { orgs::OrgsApi(self) }
    pub fn projects(&self) -> projects::ProjectsApi<'_> { projects::ProjectsApi(self) }
    pub fn server(&self) -> server::ServerApi<'_> { server::ServerApi(self) }
    pub fn user(&self) -> user::UserApi<'_> { user::UserApi(self) }

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

    pub fn build(self) -> Result<Client, &'static str> {
        let base_url = self.base_url.ok_or("base_url is required")?;
        let http = reqwest::Client::builder()
            .timeout(self.timeout.unwrap_or(Duration::from_secs(30)))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("gradient-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| "failed to build HTTP client")?;
        Ok(Client { inner: Arc::new(ClientInner { http, base_url, token: self.token }) })
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
