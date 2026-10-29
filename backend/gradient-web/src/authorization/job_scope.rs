/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::permissions::{Permission, PermissionMask, mask_from};
use crate::refusal::{Refusal, RefusalStatus};
use axum::extract::{FromRequestParts, MatchedPath, RawPathParams};
use axum::http::Method;
use axum::http::request::Parts;
use gradient_db::exec_jobs::tokens::ResolvedJobToken;

const JOB_READ_PREFIXES: &[&str] = &[
    "/projects/{project}",
    "/tasks/{project}",
    "/evals/{evaluation}",
    "/builds/{build}",
    "/exec-jobs/{job}",
];

const JOB_WRITE_ROUTES: &[&str] = &[
    "/build-requests/manifest",
    "/build-requests/{session}/blobs",
    "/build-requests/{session}/dispatch",
    "/build-requests/url",
    "/build-requests/source",
    "/build-requests/source/{upload}/chunk",
    "/build-requests/source/{upload}/finalize",
    "/evals/{evaluation}/builds",
    "/builds/{build}/log",
    "/tasks/{project}/{task}/evaluate",
    "/tasks/{project}/{task}/pull-requests",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JobScopeRefusal {
    #[error("job tokens only reach their own project and task")]
    OutsideScope,
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
    if route_reachable(method, route) && params_name_the_job(params, job) {
        Ok(())
    } else {
        Err(JobScopeRefusal::OutsideScope)
    }
}

pub(crate) async fn job_scope_of_request(
    parts: &mut Parts,
    job: &ResolvedJobToken,
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
    job_scope(&parts.method, &matched_path, &params, job)
}

fn route_reachable(method: &Method, route: &str) -> bool {
    let read = matches!(*method, Method::GET | Method::HEAD)
        && JOB_READ_PREFIXES
            .iter()
            .any(|prefix| route_is_under(route, prefix));
    read || JOB_WRITE_ROUTES.contains(&route)
}

fn route_is_under(route: &str, prefix: &str) -> bool {
    route
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
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
            (
                Method::POST,
                "/api/v1/exec-jobs/{job}/run",
                vec![("job", "j")],
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
