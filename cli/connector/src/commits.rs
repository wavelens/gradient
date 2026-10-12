use crate::api::Commit;
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct CommitsApi<'a>(pub(crate) &'a Client);

impl CommitsApi<'_> {
    pub async fn get(&self, commit: &str) -> Result<Commit, ConnectorError> {
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
