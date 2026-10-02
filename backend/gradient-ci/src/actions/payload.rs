/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde_json::Value as JsonValue;

pub fn git_host_status_payload(
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
