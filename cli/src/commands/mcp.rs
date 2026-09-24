/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::{ConfigKey, load_config};
use crate::input::client_from_config;
use crate::output::Output;
use crate::tui::watch::eval_is_terminal;
use connector::evals::EvaluationResponse;
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
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

const INSTRUCTIONS: &str = "Read-only access to a Gradient CI instance. Start from `list_projects` \
or `list_tasks`, drill into a task with `list_evaluations`, then `list_builds` on an evaluation. \
To diagnose a failure, fetch the derivation's output with `get_build_log`, or locate a message in \
a long log with `search_build_log`. Arguments named `project`, `task`, `evaluation` and `build` \
take the name or UUID as shown by the listing tools; `project` defaults to the project selected in \
the user's Gradient CLI configuration.";

const CONTROL_INSTRUCTIONS: &str = " Control tools are enabled: `start_evaluation` queues a \
run and returns its UUID, `watch_evaluation` blocks until the run finishes or times out and \
returns each entry point's build status, `abort_evaluation` cancels a run.";

const INLINE_LOG_LINES: usize = 10;
const WATCH_POLL: Duration = Duration::from_secs(5);
const WATCH_TIMEOUT_SECONDS: u64 = 600;
const ENTRY_POINT_PAGE: u64 = 500;

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
pub struct StartArgs {
    /// Task name.
    task: String,
    /// Project name or UUID. Defaults to the selected project.
    project: Option<String>,
    /// Exact 40-character commit to evaluate. Defaults to the branch head.
    commit: Option<String>,
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WatchArgs {
    /// Task name; entry points are looked up through it.
    task: String,
    /// Project name or UUID. Defaults to the selected project.
    project: Option<String>,
    /// Evaluation UUID. Defaults to the task's latest evaluation.
    evaluation: Option<String>,
    /// Seconds to wait for the evaluation to finish. Defaults to 600.
    timeout_seconds: Option<u64>,
}

#[derive(Serialize)]
struct WatchReport {
    evaluation: String,
    status: String,
    error: Option<String>,
    finished: bool,
    entry_points: Vec<EntryPointStatus>,
}

#[derive(Serialize)]
struct EntryPointStatus {
    attr: String,
    build_id: String,
    build_status: String,
}

#[derive(Clone)]
pub struct GradientMcp {
    client: Client,
    selected_project: Option<String>,
    control: bool,
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

/// Build output is read as text, not as a JSON-quoted string; a log longer than
/// `INLINE_LOG_LINES` goes to a temp file so it does not flood the client's context.
fn to_log_result(value: Result<String, ConnectorError>, start: u64) -> CallToolResult {
    let log = match value {
        Ok(log) => log,
        Err(e) => return to_error(e),
    };

    let lines = log.lines().count();
    if lines <= INLINE_LOG_LINES {
        return CallToolResult::success(vec![ContentBlock::text(log)]);
    }

    match save_log(&log) {
        Ok(path) => CallToolResult::success(vec![ContentBlock::text(format!(
            "{lines} log lines starting at line {start} saved to {}",
            path.display()
        ))]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("save log: {e}"))]),
    }
}

fn save_log(log: &str) -> std::io::Result<PathBuf> {
    let mut file = tempfile::Builder::new()
        .prefix("gradient-build-")
        .suffix(".log")
        .tempfile()?;
    file.write_all(log.as_bytes())?;
    let (_, path) = file.keep()?;
    Ok(path)
}

fn to_error(e: ConnectorError) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(e.to_string())])
}

#[tool_router(router = tool_router)]
impl GradientMcp {
    pub fn new(client: Client, selected_project: Option<String>, control: bool) -> Self {
        let mut tool_router = Self::tool_router();
        if control {
            tool_router += Self::control_router();
        }

        Self {
            client,
            selected_project,
            control,
            tool_router,
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
        description = "Read a build's log, whole or by line range. Up to 10 lines come back \
                       inline; a longer log is saved to a temp file whose path is returned."
    )]
    async fn get_build_log(
        &self,
        Parameters(args): Parameters<BuildLogArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let start = args.start.unwrap_or(1);
        let log = self
            .client
            .builds()
            .log_lines(&args.build, start, args.end)
            .await;
        Ok(to_log_result(log, start))
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

#[tool_router(router = control_router)]
impl GradientMcp {
    #[tool(description = "Start an evaluation of a task and return its UUID.")]
    async fn start_evaluation(
        &self,
        Parameters(args): Parameters<StartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project = self.project(args.project)?;
        to_result(
            self.client
                .tasks()
                .evaluate(&project, &args.task, args.commit.as_deref())
                .await,
        )
    }

    #[tool(description = "Abort an evaluation: cancels its in-progress and queued builds.")]
    async fn abort_evaluation(
        &self,
        Parameters(args): Parameters<EvaluationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        to_result(self.client.evals().abort(&args.evaluation).await)
    }

    #[tool(
        description = "Wait until an evaluation finishes (or `timeout_seconds` passes) and \
                       return its status with each entry point's build status. \
                       `finished: false` means it timed out; call again to keep waiting."
    )]
    async fn watch_evaluation(
        &self,
        Parameters(args): Parameters<WatchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project = self.project(args.project.clone())?;
        match self.watch(project, args).await {
            Ok(report) => to_result(Ok::<_, ConnectorError>(report)),
            Err(failed) => Ok(failed),
        }
    }
}

impl GradientMcp {
    async fn watch(&self, project: String, args: WatchArgs) -> Result<WatchReport, CallToolResult> {
        let evaluation = match args.evaluation {
            Some(evaluation) => evaluation,
            None => self.latest_evaluation(&project, &args.task).await?,
        };
        let timeout = Duration::from_secs(args.timeout_seconds.unwrap_or(WATCH_TIMEOUT_SECONDS));

        let eval = self
            .await_terminal(&evaluation, Instant::now() + timeout)
            .await
            .map_err(to_error)?;
        let entry_points = self
            .entry_point_statuses(&project, &args.task, &evaluation)
            .await
            .map_err(to_error)?;

        Ok(WatchReport {
            finished: eval_is_terminal(&eval.status),
            evaluation,
            status: eval.status,
            error: eval.error,
            entry_points,
        })
    }

    async fn latest_evaluation(&self, project: &str, task: &str) -> Result<String, CallToolResult> {
        let evaluations = self
            .client
            .tasks()
            .evaluations(project, task)
            .await
            .map_err(to_error)?;
        evaluations.into_iter().next().map(|e| e.id).ok_or_else(|| {
            CallToolResult::error(vec![ContentBlock::text("task has no evaluations")])
        })
    }

    async fn await_terminal(
        &self,
        evaluation: &str,
        deadline: Instant,
    ) -> Result<EvaluationResponse, ConnectorError> {
        let mut last_read = None;
        loop {
            let polled = self.client.evals().get(evaluation).await;
            if let Ok(eval) = &polled
                && eval_is_terminal(&eval.status)
            {
                return polled;
            }

            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return polled.or_else(|e| last_read.ok_or(e));
            }
            if let Ok(eval) = polled {
                last_read = Some(eval);
            }
            tokio::time::sleep(left.min(WATCH_POLL)).await;
        }
    }

    async fn entry_point_statuses(
        &self,
        project: &str,
        task: &str,
        evaluation: &str,
    ) -> Result<Vec<EntryPointStatus>, ConnectorError> {
        let mut statuses = Vec::new();
        loop {
            let page = self
                .client
                .tasks()
                .entry_points(
                    project,
                    task,
                    Some(evaluation),
                    Some(ENTRY_POINT_PAGE),
                    Some(statuses.len() as u64),
                )
                .await?;
            let fetched = page.entry_points.len();
            statuses.extend(page.entry_points.into_iter().map(|ep| EntryPointStatus {
                attr: ep.eval,
                build_id: ep.build_id,
                build_status: ep.build_status,
            }));
            if fetched == 0 || statuses.len() as u64 >= page.total {
                return Ok(statuses);
            }
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GradientMcp {
    fn get_info(&self) -> ServerInfo {
        let instructions = if self.control {
            format!("{INSTRUCTIONS}{CONTROL_INSTRUCTIONS}")
        } else {
            INSTRUCTIONS.to_string()
        };

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_server_info(Implementation::new("gradient", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions)
    }
}

/// Serves MCP over stdio, where stdout carries the JSON-RPC frames: diagnostics
/// go to stderr unconditionally, so `--json` must not reach `Output` here.
pub async fn run(control: bool) -> std::io::Result<()> {
    let out = Output::new(false);
    let client = client_from_config(out);
    let selected_project = load_config()
        .get(&ConfigKey::SelectedProject)
        .and_then(|v| v.clone())
        .filter(|p| !p.is_empty());

    let service = GradientMcp::new(client, selected_project, control)
        .serve(stdio())
        .await
        .map_err(std::io::Error::other)?;

    service.waiting().await.map_err(std::io::Error::other)?;
    Ok(())
}
