/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::client_from_config;
use crate::output::Output;

pub async fn run_gradient_search_command(query: Option<String>, limit: Option<u64>, out: Output) {
    let client = client_from_config(out);
    let hits = client
        .search()
        .find_by_query(query.as_deref().unwrap_or_default(), limit)
        .await
        .unwrap_or_else(|e| out.fail(e));

    out.ok(&hits);
    if hits.is_empty() {
        out.human("No matches found.");
    }
    for hit in &hits {
        let star = if hit.starred { "*" } else { " " };
        out.human(format!(
            "{star} {:<7} {}  {}",
            hit.kind, hit.label, hit.sublabel
        ));
    }
}
