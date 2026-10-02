/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluation_rows::{load_evaluation_rows, load_project_name};
use crate::context::CiContext;
use anyhow::{Context, Result};
use gradient_types::input::vec_to_hex;
use gradient_types::{EEvaluation, EvaluationId};
use sea_orm::EntityTrait;
use serde_json::Value as JsonValue;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EventSummary {
    pub(crate) event: String,
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) time: String,
    pub(crate) project: Option<String>,
    pub(crate) task: Option<String>,
    pub(crate) commit: Option<String>,
    pub(crate) derivation: Option<String>,
    pub(crate) link: Option<String>,
}

impl EventSummary {
    pub(crate) async fn resolve(
        ctx: &CiContext,
        event: &str,
        envelope: &JsonValue,
    ) -> Result<Self> {
        let content = envelope.get("content").unwrap_or(&JsonValue::Null);
        let text = |k: &str| {
            content
                .get(k)
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        };
        let mut summary = Self {
            event: event.to_owned(),
            id: text("build_id")
                .or_else(|| text("evaluation_id"))
                .or_else(|| text("id"))
                .unwrap_or_default(),
            status: status_of(event),
            time: envelope
                .get("at")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_owned(),
            derivation: text("derivation_path").map(|p| derivation_name(&p)),
            ..Default::default()
        };

        match text("evaluation_id") {
            Some(id) => {
                let id: EvaluationId = id.parse().context("invalid evaluation_id")?;
                summary.fill_from_evaluation(ctx, id).await?;
            }
            None => {
                summary.project = text("project");
                summary.task = text("task");
                summary.commit = text("sha").map(|s| short_sha(&s));
                summary.link = text("link");
            }
        }

        Ok(summary)
    }

    async fn fill_from_evaluation(&mut self, ctx: &CiContext, id: EvaluationId) -> Result<()> {
        let Some(evaluation) = EEvaluation::find_by_id(id)
            .one(&ctx.db.worker_db)
            .await
            .context("loading evaluation")?
        else {
            return Ok(());
        };

        if evaluation.task.is_none() {
            return Ok(());
        }

        let rows = load_evaluation_rows(ctx, &evaluation).await?;
        let project_name = load_project_name(ctx, rows.task.project).await;
        self.task = Some(rows.task.name);
        self.commit = Some(short_sha(&vec_to_hex(&rows.commit.hash)));
        self.link = project_name.as_ref().map(|project| {
            format!(
                "{}/project/{}/log/{}",
                ctx.db.config.server.frontend_url, project, evaluation.id
            )
        });
        self.project = project_name;
        Ok(())
    }

    fn headline(&self, escape: fn(&str) -> String) -> String {
        let mut line = match (&self.project, &self.task) {
            (Some(p), Some(t)) => format!("{}/{}", escape(p), escape(t)),
            (None, Some(t)) => escape(t),
            _ => escape(&self.event),
        };
        if let Some(d) = &self.derivation {
            line.push_str(&format!(": {}", escape(d)));
        }

        line.push_str(&format!(" {}", self.status));
        if let Some(c) = &self.commit {
            line.push_str(&format!(" on {}", escape(c)));
        }

        line
    }

    pub(crate) fn plain(&self) -> String {
        let line = self.headline(str::to_owned);
        match &self.link {
            Some(link) => format!("{line}\n{link}"),
            None => line,
        }
    }

    pub(crate) fn html(&self) -> String {
        let line = self.headline(escape_html);
        match &self.link {
            Some(link) => format!(
                "{line}<br><a href=\"{}\">View evaluation</a>",
                escape_html(link)
            ),
            None => line,
        }
    }

    pub(crate) fn slack(&self) -> String {
        let line = self.headline(escape_slack);
        match &self.link {
            Some(link) => format!("{line} <{}|View evaluation>", escape_slack(link)),
            None => line,
        }
    }

    pub(crate) fn mail_body(&self) -> String {
        format!(
            "{}\n\nEvent: {}\nTime: {}\nLink: {}\n",
            self.headline(str::to_owned),
            self.event,
            self.time,
            self.link.as_deref().unwrap_or(""),
        )
    }

    pub(crate) fn subject(&self, template: Option<&str>) -> String {
        template
            .unwrap_or("[Gradient] {event}: {task}")
            .replace("{event}", &self.event)
            .replace("{task}", self.task.as_deref().unwrap_or(""))
            .replace("{project}", self.project.as_deref().unwrap_or(""))
            .replace("{id}", &self.id)
            .replace("{status}", &self.status)
    }
}

fn status_of(event: &str) -> String {
    event
        .rsplit_once('.')
        .map_or(event, |(_, s)| s)
        .replace('_', " ")
}

fn escape_slack(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_html(s: &str) -> String {
    escape_slack(s).replace('"', "&quot;")
}

fn derivation_name(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let name = base.split_once('-').map_or(base, |(_, n)| n);
    name.trim_end_matches(".drv").to_owned()
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}
