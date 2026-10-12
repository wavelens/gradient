use crate::api::{
    PatchWorkerRequest, ProjectWorkerEntry, RegisterWorkerRequest, RegisterWorkerResponse,
};
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct WorkersApi<'a>(pub(crate) &'a Client);

impl WorkersApi<'_> {
    pub async fn list(&self, project: &str) -> Result<Vec<ProjectWorkerEntry>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{project}/workers"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        project: &str,
        body: RegisterWorkerRequest,
    ) -> Result<RegisterWorkerResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{project}/workers"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        project: &str,
        worker_id: &str,
        body: PatchWorkerRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("projects/{project}/workers/{worker_id}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, project: &str, worker_id: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{project}/workers/{worker_id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
