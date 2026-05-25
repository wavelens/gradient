use crate::{Client, ConnectorError, PaginatedListResponse, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProjectResponse {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub organization: String,
    pub repository: String,
    pub wildcard: String,
    pub active: bool,
    pub created_by: String,
    pub created_at: String,
    pub managed: bool,
    pub can_edit: bool,
    pub can_trigger: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeProjectRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchProjectRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub wildcard: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProjectDetails {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    pub active: bool,
    pub created_at: String,
    pub keep_evaluations: i64,
    pub last_evaluations: Vec<EvaluationSummary>,
    pub can_edit: bool,
    pub can_trigger: bool,
    pub managed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EvaluationSummary {
    pub id: String,
    pub commit: String,
    pub status: String,
    pub total_builds: i64,
    pub failed_builds: i64,
    pub completed_entry_points: i64,
    pub failed_entry_points: i64,
    pub entry_point_diff: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EntryPoint {
    pub build_id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProjectMetrics {
    pub keep_evaluations: i64,
    pub points: Vec<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EntryPointMetrics {
    pub eval: String,
    pub keep_evaluations: i64,
    pub points: Vec<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProjectIntegration {
    pub integration_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Trigger {
    pub id: String,
    pub project: String,
    #[serde(rename = "type")]
    pub trigger_type: String,
    pub config: serde_json::Value,
    pub active: bool,
    pub last_fired_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub integration: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeTriggerRequest {
    #[serde(rename = "type")]
    pub trigger_type: String,
    pub config: serde_json::Value,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchTriggerRequest {
    pub config: Option<serde_json::Value>,
    pub active: Option<bool>,
}

pub struct ProjectsApi<'a>(pub(crate) &'a Client);

impl ProjectsApi<'_> {
    pub async fn list(&self, org: &str) -> Result<PaginatedListResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn available(&self, org: &str) -> Result<PaginatedListResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/available"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, org: &str, proj: &str) -> Result<ProjectResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        org: &str,
        proj: &str,
        body: MakeProjectRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!("projects/{org}/{proj}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        org: &str,
        proj: &str,
        body: PatchProjectRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("projects/{org}/{proj}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, org: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{org}/{proj}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn details(&self, org: &str, proj: &str) -> Result<ProjectDetails, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/details"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn entry_points(
        &self,
        org: &str,
        proj: &str,
    ) -> Result<Vec<EntryPoint>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/entry-points"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn check_repository(&self, org: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{org}/{proj}/check-repository"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn evaluate(&self, org: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{org}/{proj}/evaluate"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn evaluations(
        &self,
        org: &str,
        proj: &str,
    ) -> Result<Vec<EvaluationSummary>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/evaluations"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn enable(&self, org: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{org}/{proj}/active"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn disable(&self, org: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{org}/{proj}/active"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn integration(
        &self,
        org: &str,
        proj: &str,
    ) -> Result<ProjectIntegration, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/integration"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn metrics(&self, org: &str, proj: &str) -> Result<ProjectMetrics, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/metrics"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn entry_point_metrics(
        &self,
        org: &str,
        proj: &str,
    ) -> Result<Vec<EntryPointMetrics>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/entry-point-metrics"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn entry_point_downloads(
        &self,
        org: &str,
        proj: &str,
        eval: &str,
        filename: &str,
    ) -> Result<bytes::Bytes, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/entry-point-downloads?eval={eval}&filename={filename}"),
            false,
        )?;
        let res = req.send().await?;
        let status = res.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ConnectorError::Unauthorized);
        }
        if !status.is_success() {
            return Err(ConnectorError::Api {
                status,
                message: res.text().await.unwrap_or_default(),
            });
        }
        Ok(res.bytes().await?)
    }

    pub async fn badge(&self, org: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/badge"),
            false,
        )?;
        http::decode_raw_string(req.send().await?).await
    }

    pub async fn triggers(&self, org: &str, proj: &str) -> Result<Vec<Trigger>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/triggers"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create_trigger(
        &self,
        org: &str,
        proj: &str,
        body: MakeTriggerRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{org}/{proj}/triggers"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get_trigger(
        &self,
        org: &str,
        proj: &str,
        id: &str,
    ) -> Result<Trigger, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("projects/{org}/{proj}/triggers/{id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update_trigger(
        &self,
        org: &str,
        proj: &str,
        id: &str,
        body: PatchTriggerRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("projects/{org}/{proj}/triggers/{id}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_trigger(
        &self,
        org: &str,
        proj: &str,
        id: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("projects/{org}/{proj}/triggers/{id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn test_trigger(
        &self,
        org: &str,
        proj: &str,
        id: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("projects/{org}/{proj}/triggers/{id}/test"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
