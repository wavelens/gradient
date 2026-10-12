/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use badgelib::{Badge, Color, Style};
use gradient_db::WebDb;
use gradient_db::lookup::{by_id, get_any_project_by_name};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_util::debounce::Debounce;
use http::{StatusCode, header};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::access::is_project_member;
use crate::authorization::{ApiKeyContext, MaybeApiKey, MaybeUser};
use crate::error::WebError;
use crate::helpers::OptionExt;
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use gradient_core::ServerState;
use gradient_types::*;

#[derive(Deserialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BadgeParams {
    #[serde(default = "default_style")]
    pub style: BadgeStyle,
    #[serde(default = "default_label")]
    pub label: String,
    pub eval: Option<String>,
    pub token: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Copy, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BadgeStyle {
    Flat,
    FlatSquare,
}

fn default_style() -> BadgeStyle {
    BadgeStyle::Flat
}

fn default_label() -> String {
    "gradient".to_string()
}

fn render_badge(label: &str, message: &str, color: &str, style: BadgeStyle) -> String {
    let style = match style {
        BadgeStyle::Flat => Style::Flat,
        BadgeStyle::FlatSquare => Style::FlatSquare,
    };

    Badge::new()
        .label(label)
        .label_color(Color::Hex("555".into()))
        .value(message)
        .value_color(Color::Hex(color.trim_start_matches('#').into()))
        .style(style)
        .to_svg()
}

struct BadgeContent {
    message: &'static str,
    color: &'static str,
}

fn badge_for_status(status: Option<EvaluationStatus>, has_failed_builds: bool) -> BadgeContent {
    match status {
        None => BadgeContent {
            message: "unknown",
            color: "9f9f9f",
        },
        Some(EvaluationStatus::Queued) => BadgeContent {
            message: "queued",
            color: "007ec6",
        },
        Some(EvaluationStatus::Fetching) => BadgeContent {
            message: "fetching",
            color: "007ec6",
        },
        Some(EvaluationStatus::EvaluatingFlake) | Some(EvaluationStatus::EvaluatingDerivation) => {
            BadgeContent {
                message: "evaluating",
                color: "007ec6",
            }
        }
        Some(EvaluationStatus::Building) => BadgeContent {
            message: "building",
            color: "007ec6",
        },
        Some(EvaluationStatus::Waiting) => BadgeContent {
            message: "waiting",
            color: "dfb317",
        },
        Some(EvaluationStatus::Completed) => {
            if has_failed_builds {
                BadgeContent {
                    message: "partial",
                    color: "e8a317",
                }
            } else {
                BadgeContent {
                    message: "passing",
                    color: "4c1",
                }
            }
        }
        Some(EvaluationStatus::Failed) => BadgeContent {
            message: "failing",
            color: "e05d44",
        },
        Some(EvaluationStatus::Aborted) => BadgeContent {
            message: "aborted",
            color: "dfb317",
        },
    }
}

async fn resolve_badge_user(
    db: &WebDb,
    jwt_secret: &SecretString,
    last_used_stamps: &Debounce<Uuid>,
    maybe_user: Option<MUser>,
    api_key: MaybeApiKey,
    token: Option<String>,
    client_ip: std::net::IpAddr,
) -> Result<(Option<MUser>, Option<ApiKeyContext>), WebError> {
    match token {
        Some(tok) => {
            let decoded = crate::authorization::decode_jwt(db, jwt_secret, last_used_stamps, tok)
                .await
                .map_err(|_| SharedRefusal::InvalidToken)?;
            if let Some(ctx) = decoded.api_key_context()
                && !crate::ip_allowlist::is_allowed(client_ip, &ctx.allowed_ips)
            {
                return Err(SharedRefusal::SourceIpNotAllowed.into());
            }
            let user = by_id::<EUser, _>(db, decoded.user_id()).await?;
            Ok((user, decoded.api_key_context().cloned()))
        }
        None => Ok((maybe_user, api_key.as_ref().cloned())),
    }
}

async fn check_badge_project_access(
    db: &WebDb,
    project: &MProject,
    resolved_user: &Option<MUser>,
    api_key: Option<&ApiKeyContext>,
) -> Result<(), WebError> {
    if project.public {
        return Ok(());
    }
    match resolved_user {
        Some(user) => {
            if !is_project_member(db, user.id, project.id, api_key).await? {
                return Err(WebError::not_found(Resource::Project));
            }
        }
        None => return Err(SharedRefusal::AuthorizationRequired.into()),
    }
    Ok(())
}

async fn badge_status_for_entry_point(
    db: &WebDb,
    task_id: TaskId,
    eval_attr: &str,
) -> Result<(Option<EvaluationStatus>, bool), WebError> {
    let evaluation = gradient_db::evaluations::latest_completed_of_task(db, task_id, 1)
        .await?
        .into_iter()
        .next();

    let Some(ev) = evaluation else {
        return Ok((None, false));
    };

    let Some(ep) = gradient_db::entry_points::by_attribute(db, ev.id, eval_attr).await? else {
        return Ok((None, false));
    };

    let build_status = gradient_db::derivation_builds::of_derivation(db, ep.derivation)
        .await?
        .map(|a| a.status);

    let (eval_status, has_failed) = match build_status {
        Some(BuildStatus::Completed) | Some(BuildStatus::Substituted) => {
            (EvaluationStatus::Completed, false)
        }
        Some(BuildStatus::FailedPermanent) | Some(BuildStatus::FailedTimeout) => {
            (EvaluationStatus::Failed, true)
        }
        Some(BuildStatus::Aborted) | Some(BuildStatus::DependencyFailed) => {
            (EvaluationStatus::Aborted, false)
        }
        Some(BuildStatus::Building) | Some(BuildStatus::FailedTransient) => {
            (EvaluationStatus::Building, false)
        }
        _ => (EvaluationStatus::Queued, false),
    };

    Ok((Some(eval_status), has_failed))
}

async fn badge_status_for_latest_eval(
    db: &WebDb,
    task: &MTask,
) -> Result<(Option<EvaluationStatus>, bool), WebError> {
    let Some(eval_id) = task.last_evaluation else {
        return Ok((None, false));
    };

    let eval = by_id::<EEvaluation, _>(db, eval_id).await?;

    let has_failed = match &eval {
        Some(e) if e.status == EvaluationStatus::Completed => {
            let ep_drv_ids: Vec<DerivationId> = gradient_db::entry_points::of_evaluation(db, e.id)
                .await?
                .into_iter()
                .map(|ep| ep.derivation)
                .collect();

            !ep_drv_ids.is_empty()
                && gradient_db::derivation_builds::first_with_status_among(
                    db,
                    ep_drv_ids,
                    vec![BuildStatus::FailedPermanent, BuildStatus::FailedTimeout],
                )
                .await?
                .is_some()
        }
        _ => false,
    };

    Ok((eval.map(|e| e.status), has_failed))
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/badge",
    operation_id = "getTaskBadge",
    tag = "tasks",
    summary = "Task status badge (SVG)",
    description = "Returns a shields.io-compatible SVG badge reflecting the task's latest\nevaluation status. Designed to be embedded in GitHub READMEs, Grafana\ndashboards, or any tool that renders SVG images from a URL.\n\n**Badge colours:**\n| Status | Colour | Label |\n|--------|--------|-------|\n| No evaluation yet | grey | `unknown` |\n| Queued / Evaluating / Building | blue | `queued` / `evaluating` / `building` |\n| Completed, all entry-point builds passed | green | `passing` |\n| Completed, some entry-point builds failed | amber | `partial` |\n| Failed | red | `failing` |\n| Aborted | yellow | `aborted` |\n\n**Authentication:**\n- **Public projects** - no credentials required. Embed the URL directly.\n- **Private projects** - supply `?token=GRADxxxx` (API key) or a JWT so the\n  image can be fetched by external services without a session cookie.\n\nResponses include `Cache-Control: no-cache` so badge aggregator CDNs always\nfetch the live status rather than serving a stale image.",
    params(("project" = String, Path), ("task" = String, Path), BadgeParams),
    responses((status = OK, description = "SVG badge image.", body = String, content_type = "image/svg+xml"), WebError)
)]
pub async fn get_task_badge(
    state: State<Arc<ServerState>>,
    axum::Extension(MaybeUser(maybe_user)): axum::Extension<MaybeUser>,
    axum::Extension(api_key): axum::Extension<MaybeApiKey>,
    axum::Extension(crate::client_ip::ClientIp(client_ip)): axum::Extension<
        crate::client_ip::ClientIp,
    >,
    Path((project, task)): Path<(String, String)>,
    Query(params): Query<BadgeParams>,
) -> Result<Response, WebError> {
    let project = get_any_project_by_name(&state.db(), project)
        .await?
        .or_not_found(Resource::Project)?;

    let (resolved_user, resolved_key) = resolve_badge_user(
        &state.web_db,
        &state.jwt_secret,
        &state.memory.last_used_stamps,
        maybe_user,
        api_key,
        params.token,
        client_ip,
    )
    .await?;
    check_badge_project_access(
        &state.web_db,
        &project,
        &resolved_user,
        resolved_key.as_ref(),
    )
    .await?;

    let task = gradient_db::tasks::by_name(&state.web_db, project.id, &task)
        .await?
        .or_not_found(Resource::Task)?;

    let (status, has_failed_builds) = if let Some(ref eval_attr) = params.eval {
        badge_status_for_entry_point(&state.web_db, task.id, eval_attr).await?
    } else {
        badge_status_for_latest_eval(&state.web_db, &task).await?
    };

    let content = badge_for_status(status, has_failed_builds);
    let color = format!("#{}", content.color);
    let svg = render_badge(&params.label, content.message, &color, params.style);

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "no-cache, max-age=0"),
            // Shield aggregators and CDNs are honouring these headers. They prevent stale badges
            // after an evaluation status change.
            (header::PRAGMA, "no-cache"),
        ],
        svg,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_svg_contains_label_and_message() {
        let svg = render_badge("build", "passing", "#4c1", BadgeStyle::Flat);
        let color = Color::Hex("4c1".into()).to_css();
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains("build"));
        assert!(svg.contains("passing"));
        assert!(svg.contains(&color));
    }

    #[test]
    fn badge_svg_escapes_label() {
        let svg = render_badge("<build>", "passing", "#4c1", BadgeStyle::Flat);
        assert!(svg.contains("&lt;build&gt;"));
    }

    #[test]
    fn flat_square_has_no_gradient() {
        let flat = render_badge("ci", "ok", "#4c1", BadgeStyle::Flat);
        let square = render_badge("ci", "ok", "#4c1", BadgeStyle::FlatSquare);
        assert!(flat.contains("linearGradient"));
        assert!(!square.contains("linearGradient"));
    }

    #[test]
    fn completed_with_failures_is_partial() {
        let b = badge_for_status(Some(EvaluationStatus::Completed), true);
        assert_eq!(b.message, "partial");
    }

    #[test]
    fn completed_no_failures_is_passing() {
        let b = badge_for_status(Some(EvaluationStatus::Completed), false);
        assert_eq!(b.message, "passing");
    }
}
