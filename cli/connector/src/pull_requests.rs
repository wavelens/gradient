use crate::api::{CreatePullRequestRequest, PullRequestResponse};
use crate::{Client, ConnectorError, http};
use reqwest::Method;
use std::time::Duration;

pub struct PullRequestsApi<'a>(pub(crate) &'a Client);

impl PullRequestsApi<'_> {
    pub async fn open(
        &self,
        project: &str,
        task: &str,
        body: CreatePullRequestRequest,
    ) -> Result<PullRequestResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("tasks/{project}/{task}/pull-requests"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }
}

pub async fn redact_through_agent(agent: &str, text: Vec<u8>) -> Result<Vec<u8>, ConnectorError> {
    let client = crate::http_client(reqwest::Client::builder().timeout(Duration::from_secs(30)))
        .map_err(|e| ConnectorError::Io(std::io::Error::other(e)))?;
    let res = client
        .post(format!("{}/redact", agent.trim_end_matches('/')))
        .body(text)
        .send()
        .await?;
    let status = res.status();
    let body = res.bytes().await?;
    if !status.is_success() {
        return Err(ConnectorError::Api {
            status,
            message: String::from_utf8_lossy(&body).into_owned(),
        });
    }
    Ok(body.to_vec())
}
