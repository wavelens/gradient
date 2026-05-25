use crate::{Client, ConnectorError, http};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServerConfig {
    pub oidc_enabled: bool,
    pub registration_enabled: bool,
    pub email_verification_enabled: bool,
    pub quic: bool,
}

pub struct ServerApi<'a>(pub(crate) &'a Client);

impl ServerApi<'_> {
    pub async fn get_config(&self) -> Result<ServerConfig, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            reqwest::Method::GET,
            "config",
            false,
        )?;
        http::decode(req.send().await?).await
    }
}
