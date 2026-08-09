use crate::{Client, ConnectorError, PaginatedListResponse, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TaskResponse {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub project: String,
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
pub struct MakeTaskRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub wildcard: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PatchTaskRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub wildcard: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TaskDetails {
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

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BuildStatusCounts {
    pub completed: i64,
    pub failed: i64,
    pub building: i64,
    pub queued: i64,
    pub substituted: i64,
    pub aborted: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EvaluationSummary {
    pub id: String,
    pub commit: String,
    pub commit_message: Option<String>,
    pub status: String,
    pub triggered_by: Option<String>,
    pub total_builds: i64,
    pub builds: BuildStatusCounts,
    pub errors: i64,
    pub warnings: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EntryPoint {
    pub build_id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TaskMetrics {
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
pub struct TaskIntegration {
    pub integration_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Trigger {
    pub id: String,
    pub task: String,
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

pub struct TasksApi<'a>(pub(crate) &'a Client);

impl TasksApi<'_> {
    pub async fn list(&self, project: &str) -> Result<PaginatedListResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn available(&self, project: &str) -> Result<PaginatedListResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/available"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, project: &str, proj: &str) -> Result<TaskResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        project: &str,
        body: MakeTaskRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!("tasks/{project}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        project: &str,
        proj: &str,
        body: PatchTaskRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("tasks/{project}/{proj}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(&self, project: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("tasks/{project}/{proj}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn details(&self, project: &str, proj: &str) -> Result<TaskDetails, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}/details"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn entry_points(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<Vec<EntryPoint>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}/entry-points"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn check_repository(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("tasks/{project}/{proj}/check-repository"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn evaluate(&self, project: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("tasks/{project}/{proj}/evaluate"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn evaluations(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<Vec<EvaluationSummary>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}/evaluations"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn enable(&self, project: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("tasks/{project}/{proj}/active"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn disable(&self, project: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("tasks/{project}/{proj}/active"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn integration(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<TaskIntegration, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}/integration"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn metrics(&self, project: &str, proj: &str) -> Result<TaskMetrics, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}/metrics"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn entry_point_metrics(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<Vec<EntryPointMetrics>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}/entry-point-metrics"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn entry_point_downloads(
        &self,
        project: &str,
        proj: &str,
        eval: &str,
        filename: &str,
    ) -> Result<bytes::Bytes, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "tasks/{project}/{proj}/entry-point-downloads?eval={eval}&filename={filename}"
            ),
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

    pub async fn badge(&self, project: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}/badge"),
            false,
        )?;
        http::decode_raw_string(req.send().await?).await
    }

    pub async fn triggers(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<Vec<Trigger>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}/triggers"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create_trigger(
        &self,
        project: &str,
        proj: &str,
        body: MakeTriggerRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("tasks/{project}/{proj}/triggers"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn get_trigger(
        &self,
        project: &str,
        proj: &str,
        id: &str,
    ) -> Result<Trigger, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{proj}/triggers/{id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update_trigger(
        &self,
        project: &str,
        proj: &str,
        id: &str,
        body: PatchTriggerRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!("tasks/{project}/{proj}/triggers/{id}"),
            true,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn delete_trigger(
        &self,
        project: &str,
        proj: &str,
        id: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::DELETE,
            &format!("tasks/{project}/{proj}/triggers/{id}"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn test_trigger(
        &self,
        project: &str,
        proj: &str,
        id: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("tasks/{project}/{proj}/triggers/{id}/test"),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
