// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    ActionResponse, CreateActionRequest, CreateActionResponse, DeletedResponse,
    DeliveryDetailTaskActionDeliveryId, DeliveryListItem, UpdateActionRequest,
};
use crate::http::encode_url_component;
use crate::{Client, ConnectorError, http};
use reqwest::{Method, RequestBuilder};

pub struct TaskActionsApi<'a>(pub(crate) &'a Client);

impl TaskActionsApi<'_> {
    fn request(
        &self,
        method: Method,
        project: &str,
        task: &str,
        rest: &str,
    ) -> Result<RequestBuilder, ConnectorError> {
        http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            method,
            &format!(
                "tasks/{}/{}/actions{rest}",
                encode_url_component(project),
                encode_url_component(task)
            ),
            true,
        )
    }

    pub async fn list(
        &self,
        project: &str,
        task: &str,
    ) -> Result<Vec<ActionResponse>, ConnectorError> {
        let req = self.request(Method::GET, project, task, "")?;
        http::decode(req.send().await?).await
    }

    pub async fn create(
        &self,
        project: &str,
        task: &str,
        body: &CreateActionRequest,
    ) -> Result<CreateActionResponse, ConnectorError> {
        let req = self.request(Method::POST, project, task, "")?.json(body);
        http::decode(req.send().await?).await
    }

    pub async fn get(
        &self,
        project: &str,
        task: &str,
        id: &str,
    ) -> Result<ActionResponse, ConnectorError> {
        let req = self.request(
            Method::GET,
            project,
            task,
            &format!("/{}", encode_url_component(id)),
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn update(
        &self,
        project: &str,
        task: &str,
        id: &str,
        body: &UpdateActionRequest,
    ) -> Result<ActionResponse, ConnectorError> {
        let req = self
            .request(
                Method::PATCH,
                project,
                task,
                &format!("/{}", encode_url_component(id)),
            )?
            .json(body);
        http::decode(req.send().await?).await
    }

    pub async fn delete(
        &self,
        project: &str,
        task: &str,
        id: &str,
    ) -> Result<DeletedResponse, ConnectorError> {
        let req = self.request(
            Method::DELETE,
            project,
            task,
            &format!("/{}", encode_url_component(id)),
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn list_delivery_history(
        &self,
        project: &str,
        task: &str,
        id: &str,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> Result<Vec<DeliveryListItem>, ConnectorError> {
        let mut req = self.request(
            Method::GET,
            project,
            task,
            &format!("/{}/deliveries", encode_url_component(id)),
        )?;
        if let Some(limit) = limit {
            req = req.query(&[("limit", limit)]);
        }
        if let Some(offset) = offset {
            req = req.query(&[("offset", offset)]);
        }
        http::decode(req.send().await?).await
    }

    pub async fn get_task_action_delivery_with_bodies(
        &self,
        project: &str,
        task: &str,
        id: &str,
        delivery_id: &str,
    ) -> Result<DeliveryDetailTaskActionDeliveryId, ConnectorError> {
        let req = self.request(
            Method::GET,
            project,
            task,
            &format!(
                "/{}/deliveries/{}",
                encode_url_component(id),
                encode_url_component(delivery_id)
            ),
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn test(
        &self,
        project: &str,
        task: &str,
        id: &str,
    ) -> Result<serde_json::Value, ConnectorError> {
        let req = self.request(
            Method::POST,
            project,
            task,
            &format!("/{}/test", encode_url_component(id)),
        )?;
        http::decode(req.send().await?).await
    }
}
