// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    CreateTriggerRequest, CreatedEntityResponse, EntryPointMetricsResponse, EvaluationSummary,
    MakeTaskRequest, PaginatedEntryPoints, PaginatedTaskResponse, PatchTaskRequest,
    TaskDetailsResponse, TaskMetricsResponse, TaskResponse, TransferOwnershipRequest, TriggerOut,
    UpdateTriggerRequest,
};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, http};
use reqwest::Method;

pub struct TasksApi<'a>(pub(crate) &'a Client);

impl TasksApi<'_> {
    pub async fn list(&self, project: &str) -> Result<PaginatedTaskResponse, ConnectorError> {
        let (items, total) = self
            .0
            .fetch_all_pages_and_total_count(&format!("tasks/{}", encode_url_component(project)))
            .await?;
        Ok(PaginatedTaskResponse {
            per_page: i64::try_from(items.len()).unwrap_or(i64::MAX),
            page: 1,
            total,
            items,
        })
    }

    pub async fn available(&self, project: &str, name: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{}/available", encode_url_component(project)),
            true,
        )?
        .query(&[("name", name)]);
        http::decode(req.send().await?).await
    }

    pub async fn get(&self, project: &str, proj: &str) -> Result<TaskResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "tasks/{}/{}",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        project: &str,
        body: MakeTaskRequest,
    ) -> Result<CreatedEntityResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PUT,
            &format!("tasks/{}", encode_url_component(project)),
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
            &format!(
                "tasks/{}/{}",
                encode_url_component(project),
                encode_url_component(proj)
            ),
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
            &format!(
                "tasks/{}/{}",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn details(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<TaskDetailsResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "tasks/{}/{}/details",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn entry_points(
        &self,
        project: &str,
        proj: &str,
        evaluation_id: Option<&str>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> Result<PaginatedEntryPoints, ConnectorError> {
        let mut query: Vec<String> = Vec::new();
        if let Some(evaluation_id) = evaluation_id {
            query.push(format!("evaluation_id={evaluation_id}"));
        }

        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }

        if let Some(offset) = offset {
            query.push(format!("offset={offset}"));
        }

        let query = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "tasks/{}/{}/entry-points{query}",
                encode_url_component(project),
                encode_url_component(proj)
            ),
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
            &format!(
                "tasks/{}/{}/check-repository",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn evaluate(
        &self,
        project: &str,
        proj: &str,
        commit: Option<&str>,
    ) -> Result<CreatedEntityResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!(
                "tasks/{}/{}/evaluate",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?;
        let req = match commit {
            Some(commit) => req.json(&serde_json::json!({ "commit": commit })),
            None => req,
        };
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
            &format!(
                "tasks/{}/{}/evaluations",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn list_task_evaluations_at_commit(
        &self,
        project: &str,
        task: &str,
        commit: &str,
    ) -> Result<Vec<EvaluationSummary>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("tasks/{project}/{task}/evaluations"),
            true,
        )?
        .query(&[("commit", commit)]);
        http::decode(req.send().await?).await
    }

    pub async fn enable(&self, project: &str, proj: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!(
                "tasks/{}/{}/active",
                encode_url_component(project),
                encode_url_component(proj)
            ),
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
            &format!(
                "tasks/{}/{}/active",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn metrics(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<TaskMetricsResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "tasks/{}/{}/metrics",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn entry_point_metrics(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<Vec<EntryPointMetricsResponse>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "tasks/{}/{}/entry-point-metrics",
                encode_url_component(project),
                encode_url_component(proj)
            ),
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
                "tasks/{}/{}/entry-point-downloads?eval={}&filename={}",
                encode_url_component(project),
                encode_url_component(proj),
                encode_url_component(eval),
                encode_url_component(filename)
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
            &format!(
                "tasks/{}/{}/badge",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            false,
        )?;
        http::decode_raw_string(req.send().await?).await
    }

    pub async fn triggers(
        &self,
        project: &str,
        proj: &str,
    ) -> Result<Vec<TriggerOut>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "tasks/{}/{}/triggers",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn create_trigger(
        &self,
        project: &str,
        proj: &str,
        body: CreateTriggerRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!(
                "tasks/{}/{}/triggers",
                encode_url_component(project),
                encode_url_component(proj)
            ),
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
    ) -> Result<TriggerOut, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!(
                "tasks/{}/{}/triggers/{}",
                encode_url_component(project),
                encode_url_component(proj),
                encode_url_component(id)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update_trigger(
        &self,
        project: &str,
        proj: &str,
        id: &str,
        body: UpdateTriggerRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::PATCH,
            &format!(
                "tasks/{}/{}/triggers/{}",
                encode_url_component(project),
                encode_url_component(proj),
                encode_url_component(id)
            ),
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
            &format!(
                "tasks/{}/{}/triggers/{}",
                encode_url_component(project),
                encode_url_component(proj),
                encode_url_component(id)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn move_task_to_project(
        &self,
        project: &str,
        proj: &str,
        target_project: &str,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!(
                "tasks/{}/{}/transfer",
                encode_url_component(project),
                encode_url_component(proj)
            ),
            true,
        )?
        .json(&TransferOwnershipRequest {
            project: target_project.to_owned(),
        });
        http::decode(req.send().await?).await
    }

    pub async fn set_default_trigger(
        &self,
        project: &str,
        proj: &str,
        id: &str,
    ) -> Result<TriggerOut, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!(
                "tasks/{}/{}/triggers/{}/default",
                encode_url_component(project),
                encode_url_component(proj),
                encode_url_component(id)
            ),
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
            &format!(
                "tasks/{}/{}/triggers/{}/test",
                encode_url_component(project),
                encode_url_component(proj),
                encode_url_component(id)
            ),
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
