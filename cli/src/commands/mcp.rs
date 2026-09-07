/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::{ConfigKey, load_config};
use crate::input::client_from_config;
use crate::output::Output;
use connector::{Client, ConnectorError};
use futures::StreamExt;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::transport::stdio;
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const INSTRUCTIONS: &str = "Read-only access to a Gradient CI instance. Start from `list_projects` \
or `list_tasks`, drill into a task with `list_evaluations`, then `list_builds` on an evaluation. \
To diagnose a failure, fetch the derivation's output with `get_build_log`, or locate a message in \
a long log with `search_build_log`. Arguments named `project`, `task`, `evaluation` and `build` \
take the name or UUID as shown by the listing tools; `project` defaults to the project selected in \
the user's Gradient CLI configuration.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectArgs {
    /// Project name or UUID. Defaults to the selected project.
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskArgs {
    /// Task name or UUID.
    task: String,
    /// Project name or UUID. Defaults to the selected project.
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EvaluationArgs {
    /// Evaluation UUID.
    evaluation: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildArgs {
    /// Build UUID.
    build: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildLogArgs {
    /// Build UUID.
    build: String,
    /// First log line to return, 1-based inclusive. Defaults to 1.
    start: Option<u64>,
    /// Last log line to return, inclusive. Defaults to the end of the log.
    end: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LogSearchArgs {
    /// Build UUID.
    build: String,
    /// Substring to search the log for.
    query: String,
    /// Match case-sensitively. Defaults to false.
    case_sensitive: Option<bool>,
    /// Maximum number of hits to return. Defaults to 100.
    limit: Option<usize>,
}

#[derive(Clone)]
pub struct GradientMcp {
    client: Client,
    selected_project: Option<String>,
    tool_router: ToolRouter<Self>,
}

/// A failed API call is the tool's problem, not the protocol's, so it comes
/// back as a tool-level error the MCP client renders instead of a JSON-RPC error.
fn to_result<T: Serialize>(value: Result<T, ConnectorError>) -> Result<CallToolResult, ErrorData> {
    match value {
        Ok(value) => match serde_json::to_string_pretty(&value) {
            Ok(text) => Ok(CallToolResult::success(vec![ContentBlock::text(text)])),
            Err(e) => Err(ErrorData::internal_error(format!("serialize: {e}"), None)),
        },
        Err(e) => Ok(to_error(e)),
    }
}

/// Build output is read as text, not as a JSON-quoted string.
fn to_text_result(value: Result<String, ConnectorError>) -> Result<CallToolResult, ErrorData> {
    Ok(match value {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(e) => to_error(e),
    })
}

fn to_error(e: ConnectorError) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(e.to_string())])
}

#[tool_router(router = tool_router)]
impl GradientMcp {
    pub fn new(client: Client, selected_project: Option<String>) -> Self {
        Self {
            client,
            selected_project,
            tool_router: Self::tool_router(),
        }
    }

    fn project(&self, given: Option<String>) -> Result<String, ErrorData> {
        given
            .or_else(|| self.selected_project.clone())
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    "no project given and none selected; pass `project` or run `gradient project select <name>`",
                    None,
                )
            })
    }

    #[tool(description = "List the Gradient projects the authenticated user belongs to.")]
    async fn list_projects(&self) -> Result<CallToolResult, ErrorData> {
        to_result(self.client.projects().list().await)
    }

    #[tool(description = "List the tasks of a project.")]
    async fn list_tasks(
        &self,
        Parameters(args): Parameters<ProjectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project = self.project(args.project)?;
        to_result(self.client.tasks().list(&project).await)
    }

    #[tool(
        description = "List a task's evaluations, newest first, with their build status counts."
    )]
    async fn list_evaluations(
        &self,
        Parameters(args): Parameters<TaskArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project = self.project(args.project)?;
        to_result(self.client.tasks().evaluations(&project, &args.task).await)
    }

    #[tool(description = "Get one evaluation: its commit, status and error, if it failed.")]
    async fn get_evaluation(
        &self,
        Parameters(args): Parameters<EvaluationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        to_result(self.client.evals().get(&args.evaluation).await)
    }

    #[tool(description = "List the builds of an evaluation with their per-build status.")]
    async fn list_builds(
        &self,
        Parameters(args): Parameters<EvaluationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        to_result(self.client.evals().builds(&args.evaluation).await)
    }

    #[tool(description = "Get one build: its status, derivation path, architecture and outputs.")]
    async fn get_build(
        &self,
        Parameters(args): Parameters<BuildArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        to_result(self.client.builds().get(&args.build).await)
    }

    #[tool(
        description = "Read a build's log. Returns the whole log unless a line range is given; \
                       prefer a range for long logs."
    )]
    async fn get_build_log(
        &self,
        Parameters(args): Parameters<BuildLogArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let start = args.start.unwrap_or(1);
        to_text_result(
            self.client
                .builds()
                .log_lines(&args.build, start, args.end)
                .await,
        )
    }

    #[tool(
        description = "Search a build's log and return the matching lines with their line numbers."
    )]
    async fn search_build_log(
        &self,
        Parameters(args): Parameters<LogSearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let hits = match self
            .client
            .builds()
            .log_search(
                &args.build,
                &args.query,
                args.case_sensitive.unwrap_or(false),
            )
            .await
        {
            Ok(stream) => {
                stream
                    .filter_map(|hit| async { hit.ok() })
                    .take(args.limit.unwrap_or(100))
                    .collect::<Vec<_>>()
                    .await
            }
            Err(e) => return Ok(to_error(e)),
        };

        to_result(Ok::<_, ConnectorError>(hits))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GradientMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_server_info(Implementation::new("gradient", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serves MCP over stdio, where stdout carries the JSON-RPC frames: diagnostics
/// go to stderr unconditionally, so `--json` must not reach `Output` here.
pub async fn run() -> std::io::Result<()> {
    let out = Output::new(false);
    let client = client_from_config(out);
    let selected_project = load_config()
        .get(&ConfigKey::SelectedProject)
        .and_then(|v| v.clone())
        .filter(|p| !p.is_empty());

    let service = GradientMcp::new(client, selected_project)
        .serve(stdio())
        .await
        .map_err(std::io::Error::other)?;

    service.waiting().await.map_err(std::io::Error::other)?;
    Ok(())
}
