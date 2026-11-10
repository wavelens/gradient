/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::base::parse_subcommand_name_chain_from_cli_args;
use crate::commands::poll_evaluation_until_finished::poll_evaluation_until_terminal;
use crate::config::{
    ConfigKey, load_and_migrate_login_config_file, login_name_from_flag_env_or_config,
    saved_login_for_invocation,
};
use crate::input::client_from_config;
use crate::output::Output;
use crate::tui::watch::eval_is_terminal;
use connector::{Client, ConnectorError};
use futures::StreamExt;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerConfig,
};
use rmcp::transport::stdio;
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::time::Instant;

const INSTRUCTIONS: &str = "Access to a Gradient CI instance. Start from `list_projects` or \
`list_tasks`, drill into a task with `list_evaluations`, then `list_builds` on an evaluation. To \
diagnose a failure, fetch the derivation's output with `get_build_log`, or locate a message in a long \
log with `search_build_log`. Arguments named `project`, `task`, `evaluation` and `build` take the \
name or UUID as shown by the listing tools; `project` defaults to the project selected in the user's \
Gradient CLI configuration.";

const CONTROL_INSTRUCTIONS: &str = "The control tools are enabled. `start_evaluation` can queue an \
evaluation and return its UUID. `watch_evaluation` can block until completion or timeout and return \
the build status for each entry point. `abort_evaluation` can cancel one evaluation. Create and edit \
everything else through `run_gradient_command` - projects, tasks, caches, workers, teams, webhooks and more. \
Pass CLI arguments for `gradient`. An example is [\"task\", \"create\", \"--name\", \"web\", ...]. \
The command [\"task\", \"--help\"] can list the subcommands and flags of a command group. \
`--project` and `--task` can select targets without changing the current selection.";

const ALLOWED_GRADIENT_SUBCOMMANDS: &[&str] = &[
    "project",
    "task",
    "cache",
    "worker",
    "team",
    "builds",
    "evaluation",
    "logs",
    "search",
    "status",
    "info",
    "exec-job",
];
const DENIED_GRADIENT_SUBCOMMANDS: &[&[&str]] = &[
    &["project", "secret", "set"],
    &["project", "secret", "delete"],
    &["task", "action", "create"],
    &["project", "webhook", "create"],
    &["project", "webhook", "rotate-secret"],
    &["team", "webhook", "create"],
    &["team", "webhook", "rotate-secret"],
    &["cache", "webhook", "create"],
    &["cache", "webhook", "rotate-secret"],
    &["cache", "install-netrc"],
    &["cache", "upload"],
    &["worker", "register"],
    &["team", "worker", "register"],
    &["exec-job", "download"],
    &["evaluation", "report"],
    &["builds", "sbom"],
];
const GRADIENT_COMMAND_TIMEOUT_SECONDS: Duration = Duration::from_secs(600);
const CLI_OUTPUT_SIZE_BEFORE_TEMP_FILE_BYTES: usize = 16 * 1024;

const INLINE_LOG_LINES: usize = 10;

const WATCH_TIMEOUT_SECONDS: u64 = 600;
const WATCH_MAX_TIMEOUT_SECONDS: u64 = 3600;
const ENTRY_POINT_PAGE: u64 = 500;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectArgs {
    /// Project name or UUID. The selected project is the default.
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskArgs {
    /// Task name or UUID.
    task: String,
    /// Project name or UUID. The selected project is the default.
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartArgs {
    /// Task name.
    task: String,
    /// Project name or UUID. The selected project is the default.
    project: Option<String>,
    /// Exact 40-character commit to evaluate. The branch head is the default.
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
    /// First log line to return, 1-based inclusive. 1 is the default.
    start: Option<u64>,
    /// Last log line to return, inclusive. The end of the log is the default.
    end: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LogSearchArgs {
    /// Build UUID.
    build: String,
    /// Substring to search the log for.
    query: String,
    /// Match case-sensitively. false is the default.
    case_sensitive: Option<bool>,
    /// Maximum number of hits to return. 100 is the default.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WatchArgs {
    /// Task name. Entry points are looked up through it.
    task: String,
    /// Project name or UUID. The selected project is the default.
    project: Option<String>,
    /// Evaluation UUID. The task's latest evaluation is the default.
    evaluation: Option<String>,
    /// Seconds to wait for the evaluation to finish. 600 is the default, 3600 the maximum.
    timeout_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunGradientCommandArgs {
    /// Arguments after `gradient` (example: ["task", "create", "--name", "web"]). MCP tools append `--json`.
    args: Vec<String>,
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
    cached_api_client_with_token_mutex: Arc<Mutex<(Option<String>, Client)>>,
    selected_project: Option<String>,
    account_name: Option<String>,
    control: bool,
    tool_router: ToolRouter<Self>,
}

/// A failed API call is returned as a tool-level error the MCP client is rendering. It is not a
/// JSON-RPC protocol error.
fn to_result<T: Serialize>(value: Result<T, ConnectorError>) -> Result<CallToolResult, ErrorData> {
    match value {
        Ok(value) => match serde_json::to_string(&value) {
            Ok(text) => Ok(CallToolResult::success(vec![ContentBlock::text(text)])),
            Err(e) => Err(ErrorData::internal_error(format!("serialize: {e}"), None)),
        },
        Err(e) => Ok(to_error(e)),
    }
}

fn to_log_result(value: Result<String, ConnectorError>, start: u64) -> CallToolResult {
    let log = match value {
        Ok(log) => log,
        Err(e) => return to_error(e),
    };

    let lines = log.lines().count();
    if lines <= INLINE_LOG_LINES {
        return CallToolResult::success(vec![ContentBlock::text(log)]);
    }

    match write_text_to_kept_temp_file_with_prefix_and_suffix("gradient-build-", ".log", &log) {
        Ok(path) => CallToolResult::success(vec![ContentBlock::text(format!(
            "{lines} log lines starting at line {start} saved to {}",
            path.display()
        ))]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("save log: {e}"))]),
    }
}

fn write_text_to_kept_temp_file_with_prefix_and_suffix(
    prefix: &str,
    suffix: &str,
    text: &str,
) -> std::io::Result<PathBuf> {
    let mut file = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .tempfile()?;
    file.write_all(text.as_bytes())?;
    let (_, path) = file.keep()?;
    Ok(path)
}

fn to_error(e: ConnectorError) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(e.to_string())])
}

#[tool_router(router = tool_router)]
impl GradientMcp {
    pub fn new(
        client: Client,
        selected_project: Option<String>,
        account_name: Option<String>,
        control: bool,
    ) -> Self {
        let mut tool_router = Self::tool_router();
        if control {
            tool_router += Self::control_router();
        }

        let token = saved_login_for_invocation().token();
        Self {
            cached_api_client_with_token_mutex: Arc::new(Mutex::new((token, client))),
            selected_project,
            account_name,
            control,
            tool_router,
        }
    }

    async fn renew_session_token_and_get_api_client(&self) -> Client {
        crate::session_token_renewal::renew_saved_session_token_if_half_lifetime_passed().await;
        let token = saved_login_for_invocation().token();
        let mut cached = self
            .cached_api_client_with_token_mutex
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if cached.0 != token {
            *cached = (token, client_from_config(Output::new(false)));
        }
        cached.1.clone()
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
        to_result(
            self.renew_session_token_and_get_api_client()
                .await
                .projects()
                .list()
                .await,
        )
    }

    #[tool(description = "List the tasks of a project.")]
    async fn list_tasks(
        &self,
        Parameters(args): Parameters<ProjectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project = self.project(args.project)?;
        to_result(
            self.renew_session_token_and_get_api_client()
                .await
                .tasks()
                .list(&project)
                .await,
        )
    }

    #[tool(
        description = "List a task's evaluations, newest first, with their build status counts."
    )]
    async fn list_evaluations(
        &self,
        Parameters(args): Parameters<TaskArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project = self.project(args.project)?;
        to_result(
            self.renew_session_token_and_get_api_client()
                .await
                .tasks()
                .evaluations(&project, &args.task)
                .await,
        )
    }

    #[tool(description = "Get one evaluation: its commit, status and error, if it failed.")]
    async fn get_evaluation(
        &self,
        Parameters(args): Parameters<EvaluationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        to_result(
            self.renew_session_token_and_get_api_client()
                .await
                .evals()
                .get(&args.evaluation)
                .await,
        )
    }

    #[tool(description = "List the builds of an evaluation with their per-build status.")]
    async fn list_builds(
        &self,
        Parameters(args): Parameters<EvaluationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        to_result(
            self.renew_session_token_and_get_api_client()
                .await
                .evals()
                .builds(&args.evaluation)
                .await,
        )
    }

    #[tool(description = "Get one build: its status, derivation path, architecture and outputs.")]
    async fn get_build(
        &self,
        Parameters(args): Parameters<BuildArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        to_result(
            self.renew_session_token_and_get_api_client()
                .await
                .builds()
                .get(&args.build)
                .await,
        )
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
        let client = self.renew_session_token_and_get_api_client().await;
        let builds = client.builds();
        let log = match (args.start, args.end) {
            (None, None) => builds.get_build_log_without_color_codes(&args.build).await,
            _ => builds.log_lines(&args.build, start, args.end).await,
        };
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
            .renew_session_token_and_get_api_client()
            .await
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
    #[tool(
        description = "Start an evaluation of the task. Return the evaluation UUID as `id`, the task as `name`, and the evaluation page as `url`."
    )]
    async fn start_evaluation(
        &self,
        Parameters(args): Parameters<StartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project = self.project(args.project)?;
        to_result(
            self.renew_session_token_and_get_api_client()
                .await
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
        to_result(
            self.renew_session_token_and_get_api_client()
                .await
                .evals()
                .abort(&args.evaluation)
                .await,
        )
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

    #[tool(
        description = "Run a `gradient` CLI command with `--json` and return its output. The first \
                       argument is the command group (project, task, cache, worker, team, \
                       builds, evaluation, logs, search, status, info, exec-job)."
    )]
    async fn run_gradient_command(
        &self,
        Parameters(RunGradientCommandArgs { args }): Parameters<RunGradientCommandArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let group = args.first().map(String::as_str).unwrap_or_default();
        if !ALLOWED_GRADIENT_SUBCOMMANDS.contains(&group) {
            let allowed = ALLOWED_GRADIENT_SUBCOMMANDS.join(", ");
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "`{group}` is not available here. Allowed command groups: {allowed}."
            ))]));
        }
        if let Some(chain) = parse_subcommand_name_chain_from_cli_args(&args)
            && DENIED_GRADIENT_SUBCOMMANDS
                .iter()
                .any(|denied| chain.iter().map(String::as_str).eq(denied.iter().copied()))
        {
            let command = chain.join(" ");
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "`gradient {command}` can't run here because the command can access secret values or local files."
            ))]));
        }
        if args
            .iter()
            .any(|arg| arg == "--account" || arg.starts_with("--account="))
        {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "`--account` is not available here. Commands run with the account this MCP server started with.",
            )]));
        }

        let exe =
            std::env::current_exe().map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let mut command = tokio::process::Command::new(exe);
        command
            .arg("--json")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(account) = &self.account_name {
            command.env("GRADIENT_ACCOUNT", account);
        }
        let project_given = args
            .iter()
            .any(|arg| arg == "--project" || arg.starts_with("--project="));
        if let Some(project) = self.selected_project.as_ref().filter(|_| !project_given) {
            command.arg(format!("--project={project}"));
        }

        let output =
            match tokio::time::timeout(GRADIENT_COMMAND_TIMEOUT_SECONDS, command.output()).await {
                Ok(output) => output.map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
                Err(_) => {
                    let command = args.join(" ");
                    let seconds = GRADIENT_COMMAND_TIMEOUT_SECONDS.as_secs();
                    return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "`gradient {command}` did not finish within {seconds} seconds."
                    ))]));
                }
            };

        let streams = if output.status.success() {
            vec![output.stdout]
        } else {
            vec![output.stdout, output.stderr]
        };
        let text = streams
            .iter()
            .map(|stream| String::from_utf8_lossy(stream).trim().to_owned())
            .filter(|stream| !stream.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let text = if text.len() > CLI_OUTPUT_SIZE_BEFORE_TEMP_FILE_BYTES {
            match write_text_to_kept_temp_file_with_prefix_and_suffix(
                "gradient-command-",
                ".txt",
                &text,
            ) {
                Ok(path) => {
                    let lines = text.lines().count();
                    let path = path.display();
                    format!("{lines} output lines saved to {path}")
                }
                Err(e) => return Err(ErrorData::internal_error(e.to_string(), None)),
            }
        } else {
            text
        };

        Ok(if output.status.success() {
            CallToolResult::success(vec![ContentBlock::text(text)])
        } else {
            CallToolResult::error(vec![ContentBlock::text(text)])
        })
    }
}

impl GradientMcp {
    async fn watch(&self, project: String, args: WatchArgs) -> Result<WatchReport, CallToolResult> {
        let evaluation = match args.evaluation {
            Some(evaluation) => evaluation,
            None => self.latest_evaluation(&project, &args.task).await?,
        };
        let timeout = Duration::from_secs(
            args.timeout_seconds
                .unwrap_or(WATCH_TIMEOUT_SECONDS)
                .min(WATCH_MAX_TIMEOUT_SECONDS),
        );

        self.renew_session_token_and_get_api_client()
            .await
            .tasks()
            .entry_points(&project, &args.task, Some(&evaluation), Some(1), Some(0))
            .await
            .map_err(to_error)?;
        let eval = poll_evaluation_until_terminal(
            &self.renew_session_token_and_get_api_client().await,
            &evaluation,
            Some(Instant::now() + timeout),
            |_| {},
        )
        .await
        .map_err(to_error)?;
        let entry_points = self
            .entry_point_statuses(&project, &args.task, &evaluation)
            .await
            .map_err(to_error)?;

        Ok(WatchReport {
            finished: eval_is_terminal(eval.status),
            evaluation,
            status: eval.status.to_string(),
            error: eval.error,
            entry_points,
        })
    }

    async fn latest_evaluation(&self, project: &str, task: &str) -> Result<String, CallToolResult> {
        let evaluations = self
            .renew_session_token_and_get_api_client()
            .await
            .tasks()
            .evaluations(project, task)
            .await
            .map_err(to_error)?;
        evaluations
            .into_iter()
            .next()
            .map(|e| e.id.to_string())
            .ok_or_else(|| {
                CallToolResult::error(vec![ContentBlock::text("task has no evaluations")])
            })
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
                .renew_session_token_and_get_api_client()
                .await
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
                build_id: ep.build_id.to_string(),
                build_status: ep.build_status.to_string(),
            }));
            if fetched == 0 || i64::try_from(statuses.len()).is_ok_and(|seen| seen >= page.total) {
                return Ok(statuses);
            }
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GradientMcp {
    fn get_info(&self) -> ServerConfig {
        let instructions = if self.control {
            format!("{INSTRUCTIONS} {CONTROL_INSTRUCTIONS}")
        } else {
            INSTRUCTIONS.to_string()
        };

        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_server_info(Implementation::new("gradient", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions)
    }
}

/// stdout is carrying the JSON-RPC frames. Diagnostics are going to stderr unconditionally, and
/// `--json` must not reach `Output` here.
pub async fn run(control: bool) -> std::io::Result<()> {
    let out = Output::new(false);
    let client = client_from_config(out);
    let selected_project = saved_login_for_invocation().get(ConfigKey::SelectedProject);
    let account_name = login_name_from_flag_env_or_config(&load_and_migrate_login_config_file());

    let service = GradientMcp::new(client, selected_project, account_name, control)
        .serve(stdio())
        .await
        .map_err(std::io::Error::other)?;

    service.waiting().await.map_err(std::io::Error::other)?;
    Ok(())
}
