/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Project status badge endpoint.
//!
//! `GET /api/v1/projects/{org}/{project}/badge` returns a shields.io-compatible
//! SVG badge reflecting the project's latest evaluation status. Private
//! organisations require a `?token=GRADxxxx` or JWT (same mechanism as the
//! entry-point download endpoint).
//!
//! Supported query parameters:
//! - `style`: `flat` (default) or `flat-square`
//! - `label`: left-hand label text (default `"build"`)
//! - `token`: API key or JWT for private organisations

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use http::{StatusCode, header};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Deserialize;
use std::sync::Arc;

use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::WebError;
use core::database::get_any_organization_by_name;
use core::types::*;

// ── Query parameters ─────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
pub struct BadgeParams {
    /// Shield style. `flat` (default) gives rounded corners; `flat-square` has none.
    #[serde(default = "default_style")]
    pub style: BadgeStyle,
    /// Left-side label. Defaults to `"build"`.
    #[serde(default = "default_label")]
    pub label: String,
    /// API key (`GRADxxxx`) or JWT for accessing a private organisation badge
    /// without a session. Embed in the image URL so external services (GitHub
    /// README, Grafana, …) can fetch it without interactive login.
    pub token: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum BadgeStyle {
    Flat,
    FlatSquare,
}

fn default_style() -> BadgeStyle {
    BadgeStyle::Flat
}

fn default_label() -> String {
    "build".to_string()
}

// ── SVG generation ────────────────────────────────────────────────────────────

/// Character width table for **Verdana 11 px** in units of 0.1 px.
///
/// Indexed by ASCII code starting at 32 (space). Source: the same table used
/// by shields.io / badge-maker so our text placement is compatible.
#[rustfmt::skip]
const CHAR_WIDTHS: &[u16] = &[
    //  sp   !    "    #    $    %    &    '    (    )    *    +    ,    -    .    /
        33,  40,  50,  90,  70, 100,  90,  30,  40,  40,  70,  90,  40,  50,  40,  50,
    //  0    1    2    3    4    5    6    7    8    9    :    ;    <    =    >    ?
        70,  70,  70,  70,  70,  70,  70,  70,  70,  70,  40,  40,  90,  90,  90,  60,
    //  @    A    B    C    D    E    F    G    H    I    J    K    L    M    N    O
       120,  90,  80,  80,  90,  70,  60,  90,  90,  40,  40,  80,  60, 110,  90, 100,
    //  P    Q    R    S    T    U    V    W    X    Y    Z    [    \    ]    ^    _
        70, 100,  80,  70,  80,  90,  90, 140,  90,  90,  80,  40,  50,  40,  90,  70,
    //  `    a    b    c    d    e    f    g    h    i    j    k    l    m    n    o
        40,  70,  80,  60,  80,  70,  40,  80,  80,  40,  40,  70,  40, 110,  80,  80,
    //  p    q    r    s    t    u    v    w    x    y    z
        80,  80,  50,  60,  50,  80,  70, 100,  70,  70,  60,
];

/// Returns the width of `c` in 0.1 px units using the Verdana 11 px table.
fn char_width(c: char) -> u32 {
    let idx = c as usize;
    if idx >= 32 && (idx - 32) < CHAR_WIDTHS.len() {
        CHAR_WIDTHS[idx - 32] as u32
    } else {
        70 // fallback ~7 px
    }
}

/// Total text width of `s` in whole pixels (rounded up).
fn text_width_px(s: &str) -> u32 {
    let tenths: u32 = s.chars().map(char_width).sum();
    (tenths + 9) / 10 // ceil
}

/// Render a shields.io-compatible flat SVG badge.
///
/// Uses string concatenation rather than a raw-string template because
/// SVG hex colours like `fill="#fff"` contain the sequence `"#` which
/// would prematurely terminate a `r#"..."#` raw string literal.
fn render_badge(label: &str, message: &str, color: &str, style: BadgeStyle) -> String {
    let lw = text_width_px(label) + 10; // 5 px padding each side
    let rw = text_width_px(message) + 10;
    let total = lw + rw;

    // Text anchors in the SVG's 10× scaled coordinate space.
    let lx = lw * 5;            // centre of left half
    let rx = lw * 10 + rw * 5; // centre of right half

    // Text "printed length" in the 10× space (for textLength attribute).
    let ltl = (text_width_px(label) * 10).max(1);
    let rtl = (text_width_px(message) * 10).max(1);

    let rx_attr = match style {
        BadgeStyle::Flat => 3,
        BadgeStyle::FlatSquare => 0,
    };

    // Strings assembled without raw literals to avoid `"#` collision.
    let gradient_defs = match style {
        BadgeStyle::Flat => [
            "<linearGradient id=\"s\" x2=\"0\" y2=\"100%\">",
            "<stop offset=\"0\" stop-color=\"#bbb\" stop-opacity=\".1\"/>",
            "<stop offset=\"1\" stop-opacity=\".1\"/>",
            "</linearGradient>",
        ]
        .concat(),
        BadgeStyle::FlatSquare => String::new(),
    };

    let gradient_rect = match style {
        BadgeStyle::Flat => format!(
            "<rect width=\"{total}\" height=\"20\" fill=\"url(#s)\"/>"
        ),
        BadgeStyle::FlatSquare => String::new(),
    };

    [
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{total}\" height=\"20\" role=\"img\" aria-label=\"{label}: {message}\">"),
        format!("<title>{label}: {message}</title>"),
        format!("<defs>{gradient_defs}<clipPath id=\"r\"><rect width=\"{total}\" height=\"20\" rx=\"{rx_attr}\" fill=\"#fff\"/></clipPath></defs>"),
        format!("<g clip-path=\"url(#r)\">"),
        format!("<rect width=\"{lw}\" height=\"20\" fill=\"#555\"/>"),
        format!("<rect x=\"{lw}\" width=\"{rw}\" height=\"20\" fill=\"{color}\"/>"),
        gradient_rect,
        "</g>".to_string(),
        "<g fill=\"#fff\" text-anchor=\"middle\" font-family=\"DejaVu Sans,Verdana,Geneva,sans-serif\" font-size=\"110\">".to_string(),
        format!("<text aria-hidden=\"true\" x=\"{lx}\" y=\"150\" fill=\"#010101\" fill-opacity=\".3\" transform=\"scale(.1)\" textLength=\"{ltl}\" lengthAdjust=\"spacing\">{label}</text>"),
        format!("<text x=\"{lx}\" y=\"140\" transform=\"scale(.1)\" textLength=\"{ltl}\" lengthAdjust=\"spacing\">{label}</text>"),
        format!("<text aria-hidden=\"true\" x=\"{rx}\" y=\"150\" fill=\"#010101\" fill-opacity=\".3\" transform=\"scale(.1)\" textLength=\"{rtl}\" lengthAdjust=\"spacing\">{message}</text>"),
        format!("<text x=\"{rx}\" y=\"140\" transform=\"scale(.1)\" textLength=\"{rtl}\" lengthAdjust=\"spacing\">{message}</text>"),
        "</g></svg>".to_string(),
    ]
    .join("")
}

// ── Badge content from evaluation status ──────────────────────────────────────

struct BadgeContent {
    message: &'static str,
    /// 6-digit hex colour without `#`.
    color: &'static str,
}

fn badge_for_status(status: Option<EvaluationStatus>, has_failed_builds: bool) -> BadgeContent {
    match status {
        None => BadgeContent { message: "unknown", color: "9f9f9f" },
        Some(EvaluationStatus::Queued) => BadgeContent { message: "queued", color: "007ec6" },
        Some(EvaluationStatus::Evaluating) => BadgeContent { message: "evaluating", color: "007ec6" },
        Some(EvaluationStatus::Building) => BadgeContent { message: "building", color: "007ec6" },
        Some(EvaluationStatus::Completed) => {
            if has_failed_builds {
                BadgeContent { message: "partial", color: "e8a317" }
            } else {
                BadgeContent { message: "passing", color: "4c1" }
            }
        }
        Some(EvaluationStatus::Failed) => BadgeContent { message: "failing", color: "e05d44" },
        Some(EvaluationStatus::Aborted) => BadgeContent { message: "aborted", color: "dfb317" },
    }
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// Returns a shields.io-compatible SVG status badge for the named project.
///
/// For public organisations the badge is accessible without credentials.
/// For private organisations supply `?token=GRADxxxx` (an API key) or a JWT
/// so the URL can be embedded in external tools (GitHub README, Grafana …)
/// without exposing a session cookie.
pub async fn get_project_badge(
    state: State<Arc<ServerState>>,
    axum::Extension(MaybeUser(maybe_user)): axum::Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
    Query(params): Query<BadgeParams>,
) -> Result<Response, WebError> {
    let organization = get_any_organization_by_name(state.0.clone(), organization)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))?;

    // Resolve caller identity from ?token= or existing session.
    let resolved_user: Option<MUser> = if let Some(tok) = params.token {
        let token_data =
            crate::authorization::decode_jwt(State(Arc::clone(&state)), tok)
                .await
                .map_err(|_| WebError::Unauthorized("Invalid token".to_string()))?;
        EUser::find_by_id(token_data.claims.id)
            .one(&state.db)
            .await?
    } else {
        maybe_user
    };

    if !organization.public {
        match resolved_user {
            Some(ref user) => {
                if !user_is_org_member(&state, user.id, organization.id).await? {
                    return Err(WebError::not_found("Organization"));
                }
            }
            None => return Err(WebError::Unauthorized("Authorization required".to_string())),
        }
    }

    // Look up the project.
    let project = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(&project))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Project"))?;

    // Determine badge content from the last evaluation.
    let (status, has_failed_builds) = if let Some(eval_id) = project.last_evaluation {
        let eval = EEvaluation::find_by_id(eval_id)
            .one(&state.db)
            .await?;

        let has_failed = match &eval {
            Some(e) if e.status == EvaluationStatus::Completed => {
                // Check whether any entry-point build failed.
                let ep_build_ids: Vec<uuid::Uuid> = EEntryPoint::find()
                    .filter(CEntryPoint::Evaluation.eq(e.id))
                    .all(&state.db)
                    .await?
                    .into_iter()
                    .map(|ep| ep.build)
                    .collect();

                if ep_build_ids.is_empty() {
                    false
                } else {
                    EBuild::find()
                        .filter(CBuild::Id.is_in(ep_build_ids))
                        .filter(CBuild::Status.eq(BuildStatus::Failed))
                        .one(&state.db)
                        .await?
                        .is_some()
                }
            }
            _ => false,
        };

        (eval.map(|e| e.status), has_failed)
    } else {
        (None, false)
    };

    let content = badge_for_status(status, has_failed_builds);
    let color = format!("#{}", content.color);
    let svg = render_badge(&params.label, content.message, &color, params.style);

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "no-cache, max-age=0"),
            // Shield aggregators and CDNs respect these; they prevent stale
            // badges from being served even when the evaluation status changes.
            (header::PRAGMA, "no-cache"),
        ],
        svg,
    )
        .into_response())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_width_non_zero() {
        assert!(text_width_px("build") > 0);
        assert!(text_width_px("passing") > text_width_px("ok"));
    }

    #[test]
    fn badge_svg_contains_label_and_message() {
        let svg = render_badge("build", "passing", "#4c1", BadgeStyle::Flat);
        assert!(svg.contains("build"));
        assert!(svg.contains("passing"));
        assert!(svg.contains("#4c1"));
        assert!(svg.contains("image/svg+xml") || svg.contains("svg"));
    }

    #[test]
    fn flat_square_has_no_gradient() {
        let flat = render_badge("ci", "ok", "#4c1", BadgeStyle::Flat);
        let square = render_badge("ci", "ok", "#4c1", BadgeStyle::FlatSquare);
        assert!(flat.contains("linearGradient"));
        assert!(!square.contains("linearGradient"));
    }

    #[test]
    fn badge_for_none_is_unknown() {
        let b = badge_for_status(None, false);
        assert_eq!(b.message, "unknown");
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
