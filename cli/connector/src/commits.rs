use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommitResponse {
    pub id: String,
    pub message: String,
    pub hash: String,
    pub author: Option<String>,
    pub author_name: String,
}

pub struct CommitsApi<'a>(pub(crate) &'a Client);

impl CommitsApi<'_> {
    pub async fn get(&self, commit: &str) -> Result<CommitResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("commits/{commit}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
