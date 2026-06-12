/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::client_from_config;
use crate::output::{ExitKind, Output, to_exit_kind};
use futures::{StreamExt, pin_mut};

/// Parse a `--lines` argument: `L120-L130`, `120-130`, or `120`.
fn parse_lines(arg: &str) -> Option<(u64, Option<u64>)> {
    let cleaned = arg.replace(['L', 'l'], "");
    if let Some((lo, hi)) = cleaned.split_once('-') {
        Some((lo.trim().parse().ok()?, Some(hi.trim().parse().ok()?)))
    } else {
        Some((cleaned.trim().parse().ok()?, None))
    }
}

pub async fn handle_log(
    id: &str,
    interactive: bool,
    lines: Option<String>,
    search: Option<String>,
    case: bool,
    out: Output,
) {
    let client = client_from_config(out);

    if let Some(arg) = lines {
        let Some((start, end)) = parse_lines(&arg) else {
            out.err(ExitKind::Api, "invalid --lines range (use L120-L130)");
        };
        match client.builds().log_lines(id, start, end).await {
            Ok(text) => {
                if out.is_json() {
                    println!("{}", serde_json::json!({"error": false, "message": text}));
                } else {
                    print!("{text}");
                }
            }
            Err(e) => out.err(to_exit_kind(&e), e),
        }
        return;
    }

    if let Some(q) = search {
        let stream = match client.builds().log_search(id, &q, case).await {
            Ok(s) => s,
            Err(e) => out.err(to_exit_kind(&e), e),
        };
        pin_mut!(stream);
        while let Some(item) = stream.next().await {
            match item {
                Ok(value) => {
                    if value.get("done").and_then(|d| d.as_bool()) == Some(true) {
                        if !out.is_json() {
                            let n = value.get("total_matches").and_then(|n| n.as_u64()).unwrap_or(0);
                            out.human(format!("{n} match(es)"));
                        }
                        break;
                    }
                    if out.is_json() {
                        println!("{value}");
                    } else {
                        let line = value.get("line_number").and_then(|n| n.as_u64()).unwrap_or(0);
                        let preview = value.get("preview").and_then(|p| p.as_str()).unwrap_or("");
                        println!("{line}: {preview}");
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }
        return;
    }

    let stream = match client.builds().log_stream(id).await {
        Ok(s) => s,
        Err(e) => out.err(to_exit_kind(&e), e),
    };

    if interactive && !out.is_json() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            pin_mut!(stream);
            while let Some(item) = stream.next().await {
                if let Ok(line) = item
                    && tx.send(line).is_err()
                {
                    break;
                }
            }
        });
        crate::tui::run(crate::tui::log_view::LogView::streaming(rx))
            .unwrap_or_else(|e| out.err(ExitKind::Api, format!("tui error: {e}")));
    } else {
        pin_mut!(stream);
        while let Some(item) = stream.next().await {
            match item {
                Ok(line) => {
                    if out.is_json() {
                        println!("{}", serde_json::json!({"error": false, "message": line}));
                    } else {
                        print!("{}", line);
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }
    }
}
