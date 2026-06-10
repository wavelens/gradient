/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde_json::Value as JsonValue;

/// Builds the payload skeleton expected by `execute_forge_status_report`.
pub fn forge_status_payload(
    owner: &str,
    repo: &str,
    sha: &str,
    context: &str,
    description: Option<&str>,
    details_url: Option<&str>,
    check_run_id: Option<i64>,
) -> JsonValue {
    let mut v = serde_json::json!({
        "owner": owner,
        "repo": repo,
        "sha": sha,
        "context": context,
    });
    if let Some(d) = description {
        v["description"] = JsonValue::String(d.into());
    }
    if let Some(u) = details_url {
        v["details_url"] = JsonValue::String(u.into());
    }
    if let Some(id) = check_run_id {
        v["check_run_id"] = JsonValue::from(id);
    }
    v
}

pub(super) fn render_subject(template: Option<&str>, event: &str, payload: &JsonValue) -> String {
    let raw = template.unwrap_or("[Gradient] {event}: {project}");
    let get = |k: &str| {
        payload
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    raw.replace("{event}", event)
        .replace("{project}", &get("project"))
        .replace("{org}", &get("org"))
        .replace("{id}", &get("id"))
        .replace("{status}", &get("status"))
}

pub(super) fn render_default_body(event: &str, payload: &JsonValue) -> String {
    let get = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("");
    format!(
        "Event: {}\nProject: {}/{}\nEntity: {}\nStatus: {}\nTime: {}\nLink: {}\n",
        event,
        get("org"),
        get("project"),
        get("id"),
        get("status"),
        get("time"),
        get("link"),
    )
}
