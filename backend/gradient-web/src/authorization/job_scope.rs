/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::api_key::DecodedRequest;
use crate::permissions::{Permission, PermissionMask, mask_from};
use crate::refusal::{Refusal, RefusalStatus};
use axum::extract::{FromRequestParts, MatchedPath, RawPathParams};
use axum::http::Method;
use axum::http::request::Parts;
use gradient_db::WebDb;
use gradient_db::exec_jobs::tokens::ResolvedJobToken;
use gradient_db::lookup::by_id;
use gradient_types::{EEvaluation, EvaluationId};

const JOB_READ_ROUTES: &[&str] = &[
    "/projects/{project}",
    "/tasks/{project}/{task}",
    "/tasks/{project}/{task}/badge",
    "/tasks/{project}/{task}/details",
    "/tasks/{project}/{task}/entry-point-downloads",
    "/tasks/{project}/{task}/entry-point-metrics",
    "/tasks/{project}/{task}/entry-points",
    "/tasks/{project}/{task}/evaluations",
    "/tasks/{project}/{task}/flake-inputs",
    "/tasks/{project}/{task}/flake-inputs/{id}",
    "/tasks/{project}/{task}/live",
    "/tasks/{project}/{task}/metrics",
    "/evals/{evaluation}",
    "/evals/{evaluation}/artefacts",
    "/evals/{evaluation}/builds",
    "/evals/{evaluation}/closure",
    "/evals/{evaluation}/exec",
    "/evals/{evaluation}/flake-graph",
    "/evals/{evaluation}/live",
    "/evals/{evaluation}/messages",
    "/evals/{evaluation}/report",
    "/evals/{evaluation}/runtime-closure",
    "/evals/{evaluation}/summary",
    "/builds/{build}",
    "/builds/{build}/attempts",
    "/builds/{build}/closure",
    "/builds/{build}/download-token",
    "/builds/{build}/download/{filename}",
    "/builds/{build}/downloads",
    "/builds/{build}/graph",
    "/builds/{build}/live",
    "/builds/{build}/log",
    "/builds/{build}/log/chunk/{index}",
    "/builds/{build}/log/chunks",
    "/builds/{build}/log/lines",
    "/builds/{build}/log/search",
    "/builds/{build}/runtime-closure",
    "/builds/{build}/sbom",
    "/exec/{job}",
    "/exec/{job}/log",
    "/exec/{job}/outputs",
    "/exec/{job}/outputs/{path}",
    "/exec/{job}/pull-requests",
];

const JOB_BUILD_REQUEST_ROUTES: &[&str] = &[
    "/build-requests/manifest",
    "/build-requests/{session}/blobs",
    "/build-requests/{session}/dispatch",
    "/build-requests/url",
    "/build-requests/source",
    "/build-requests/source/{upload}/chunk",
    "/build-requests/source/{upload}/finalize",
];

const JOB_WRITE_ROUTES: &[&str] = &[
    "/evals/{evaluation}/builds",
    "/builds/{build}/log",
    "/tasks/{project}/{task}/evaluate",
    "/tasks/{project}/{task}/pull-requests",
];

const ROUTES_FOR_JOB_CREATED_EVALUATION_WITHOUT_PROJECT_CREDENTIALS: &[(Method, &str)] = &[
    (Method::POST, "/evals/{evaluation}/builds"),
    (Method::GET, "/evals/{evaluation}"),
    (Method::GET, "/evals/{evaluation}/artefacts"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JobScopeRefusal {
    #[error("job tokens only reach their own project and task")]
    OutsideScope,
    #[error(
        "job tokens without project credentials for evaluations reach only build requests and the evaluations that those build requests started"
    )]
    EvaluationWithoutProjectCredentialsOrPrivilegedActions,
}

impl Refusal for JobScopeRefusal {
    fn status(&self) -> RefusalStatus {
        RefusalStatus::Forbidden
    }
}

pub fn job_mask() -> PermissionMask {
    mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation])
}

pub(crate) fn job_scope(
    method: &Method,
    matched_path: &str,
    params: &[(&str, &str)],
    job: &ResolvedJobToken,
) -> Result<(), JobScopeRefusal> {
    let route = matched_path.strip_prefix("/api/v1").unwrap_or(matched_path);
    if !(route_reachable(method, route) && params_name_the_job(params, job)) {
        return Err(JobScopeRefusal::OutsideScope);
    }
    if job.allows_project_credentials_and_privileged_actions
        || JOB_BUILD_REQUEST_ROUTES.contains(&route)
    {
        Ok(())
    } else {
        Err(JobScopeRefusal::EvaluationWithoutProjectCredentialsOrPrivilegedActions)
    }
}

pub(crate) fn reject_job_token_without_project_credentials_or_privileged_actions(
    decoded: &DecodedRequest,
) -> Result<(), JobScopeRefusal> {
    match decoded.job() {
        Some(job) if !job.allows_project_credentials_and_privileged_actions => {
            Err(JobScopeRefusal::EvaluationWithoutProjectCredentialsOrPrivilegedActions)
        }
        _ => Ok(()),
    }
}

async fn evaluation_was_created_by_token_job(
    db: &WebDb,
    method: &Method,
    matched_path: &str,
    params: &[(&str, &str)],
    job: &ResolvedJobToken,
) -> bool {
    let route = matched_path.strip_prefix("/api/v1").unwrap_or(matched_path);
    if !ROUTES_FOR_JOB_CREATED_EVALUATION_WITHOUT_PROJECT_CREDENTIALS
        .iter()
        .any(|(allowed_method, allowed_route)| allowed_method == method && *allowed_route == route)
    {
        return false;
    }
    let Some(evaluation) = params
        .iter()
        .find(|(name, _)| *name == "evaluation")
        .and_then(|(_, value)| value.parse::<EvaluationId>().ok())
    else {
        return false;
    };
    matches!(
        by_id::<EEvaluation, _>(db, evaluation).await,
        Ok(Some(evaluation)) if evaluation.parent_exec_job == Some(job.job)
    )
}

pub(crate) async fn job_scope_of_request(
    parts: &mut Parts,
    job: &ResolvedJobToken,
    db: &WebDb,
) -> Result<(), JobScopeRefusal> {
    let matched_path = parts
        .extensions
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned())
        .ok_or(JobScopeRefusal::OutsideScope)?;
    let params = RawPathParams::from_request_parts(parts, &())
        .await
        .map_err(|_| JobScopeRefusal::OutsideScope)?;
    let params: Vec<(&str, &str)> = params.iter().collect();
    match job_scope(&parts.method, &matched_path, &params, job) {
        Err(JobScopeRefusal::EvaluationWithoutProjectCredentialsOrPrivilegedActions)
            if evaluation_was_created_by_token_job(
                db,
                &parts.method,
                &matched_path,
                &params,
                job,
            )
            .await =>
        {
            Ok(())
        }
        scope => scope,
    }
}

fn route_reachable(method: &Method, route: &str) -> bool {
    let read = matches!(*method, Method::GET | Method::HEAD) && JOB_READ_ROUTES.contains(&route);
    read || JOB_WRITE_ROUTES.contains(&route) || JOB_BUILD_REQUEST_ROUTES.contains(&route)
}

fn params_name_the_job(params: &[(&str, &str)], job: &ResolvedJobToken) -> bool {
    params.iter().all(|(name, value)| match *name {
        "project" => *value == job.project_name || *value == job.project.to_string(),
        "task" => *value == job.task_name || *value == job.task.to_string(),
        _ => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::{ExecJobAttemptId, ExecJobId, ProjectId, TaskId, UserId};

    fn job() -> ResolvedJobToken {
        ResolvedJobToken {
            job: ExecJobId::now_v7(),
            attempt: ExecJobAttemptId::now_v7(),
            project: ProjectId::now_v7(),
            task: TaskId::now_v7(),
            project_name: "infra".into(),
            task_name: "master".into(),
            acting_user: UserId::now_v7(),
            allows_project_credentials_and_privileged_actions: true,
        }
    }

    #[test]
    fn an_evaluation_job_token_without_project_credentials_is_refused_on_every_read_and_write_route_and_reaches_only_build_request_routes()
     {
        let job = ResolvedJobToken {
            allows_project_credentials_and_privileged_actions: false,
            ..job()
        };
        let params = [("project", "infra"), ("task", "master")];
        for route in JOB_READ_ROUTES {
            assert_eq!(
                job_scope(&Method::GET, route, &params, &job),
                Err(JobScopeRefusal::EvaluationWithoutProjectCredentialsOrPrivilegedActions),
                "{route}"
            );
        }
        for route in JOB_WRITE_ROUTES {
            assert_eq!(
                job_scope(&Method::POST, route, &params, &job),
                Err(JobScopeRefusal::EvaluationWithoutProjectCredentialsOrPrivilegedActions),
                "{route}"
            );
        }
        for route in JOB_BUILD_REQUEST_ROUTES {
            assert_eq!(
                job_scope(&Method::POST, route, &params, &job),
                Ok(()),
                "{route}"
            );
        }
    }

    #[test]
    fn a_job_reaches_its_task_reads_and_build_requests() {
        let job = job();
        let task_id = job.task.to_string();
        for (method, path, params) in [
            (
                Method::GET,
                "/api/v1/projects/{project}",
                vec![("project", "infra")],
            ),
            (
                Method::GET,
                "/api/v1/evals/{evaluation}/builds",
                vec![("evaluation", "e")],
            ),
            (
                Method::POST,
                "/api/v1/evals/{evaluation}/builds",
                vec![("evaluation", "e")],
            ),
            (Method::POST, "/api/v1/build-requests/manifest", vec![]),
            (
                Method::POST,
                "/api/v1/tasks/{project}/{task}/pull-requests",
                vec![("project", "infra"), ("task", "master")],
            ),
            (
                Method::GET,
                "/tasks/{project}/{task}",
                vec![("project", "infra"), ("task", task_id.as_str())],
            ),
        ] {
            assert!(
                job_scope(&method, path, &params, &job).is_ok(),
                "{method} {path}"
            );
        }
    }

    #[test]
    fn a_job_is_refused_outside_its_project_task_and_on_account_routes() {
        for (method, path, params) in [
            (Method::GET, "/api/v1/user", vec![]),
            (Method::GET, "/api/v1/workers", vec![]),
            (
                Method::GET,
                "/api/v1/tasks/{project}",
                vec![("project", "infra")],
            ),
            (
                Method::GET,
                "/api/v1/tasks/{project}/available",
                vec![("project", "infra")],
            ),
            (
                Method::GET,
                "/api/v1/projects/{project}",
                vec![("project", "other")],
            ),
            (
                Method::GET,
                "/api/v1/projects-overview/{project}",
                vec![("project", "infra")],
            ),
            (
                Method::POST,
                "/api/v1/tasks/{project}/{task}/pull-requests",
                vec![("project", "infra"), ("task", "nightly")],
            ),
            (
                Method::PATCH,
                "/api/v1/tasks/{project}/{task}",
                vec![("project", "infra"), ("task", "master")],
            ),
            (
                Method::POST,
                "/api/v1/evals/{evaluation}",
                vec![("evaluation", "e")],
            ),
            (Method::POST, "/api/v1/exec/{job}/run", vec![("job", "j")]),
            (Method::GET, "/api/v1/exec/{job}/shell", vec![("job", "j")]),
            (
                Method::GET,
                "/api/v1/exec/{job}/console",
                vec![("job", "j")],
            ),
            (
                Method::GET,
                "/api/v1/exec/{job}/forward",
                vec![("job", "j")],
            ),
            (
                Method::GET,
                "/api/v1/projects/{project}/secrets",
                vec![("project", "infra")],
            ),
        ] {
            assert!(
                matches!(
                    job_scope(&method, path, &params, &job()),
                    Err(JobScopeRefusal::OutsideScope)
                ),
                "{method} {path}"
            );
        }
    }
}
