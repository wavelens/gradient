/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::logfmt;
use crate::output::Output;
use connector::evals::EvalMessage;
use futures::StreamExt;
use futures::pin_mut;
use std::collections::HashSet;
use std::io::Write as _;
use std::time::Duration;

/// Stream an evaluation's full build log - the server replays every build's
/// stored log, then follows the active ones - while concurrently surfacing the
/// eval-level messages (warnings/errors) that the build-log stream does not
/// carry. Build output and messages are colour-coded for a TTY. Shared by
/// `gradient build`, `gradient logs`, and `gradient project log`.
pub async fn stream_eval_logs(client: &connector::Client, evaluation: &str, out: Output) {
    let evals = client.evals();
    let stream = match evals.stream_builds(evaluation).await {
        Ok(s) => s,
        Err(e) => {
            out.progress(format!("Failed to stream logs: {e}"));
            return;
        }
    };
    pin_mut!(stream);

    let mut seen: HashSet<String> = HashSet::new();
    let mut messages = tokio::time::interval(Duration::from_millis(1500));

    loop {
        tokio::select! {
            item = stream.next() => match item {
                Some(Ok(line)) => print_log(&line, out),
                Some(Err(e)) => {
                    out.progress(format!("log stream error: {e}"));
                    break;
                }
                None => break,
            },
            _ = messages.tick() => flush_messages(client, evaluation, &mut seen, out).await,
        }
    }
    flush_messages(client, evaluation, &mut seen, out).await;
}

fn print_log(line: &str, out: Output) {
    if out.is_json() {
        println!("{}", serde_json::json!({"error": false, "message": line}));
    } else {
        print!("{}", logfmt::render_log(line));
        let _ = std::io::stdout().flush();
    }
}

async fn flush_messages(
    client: &connector::Client,
    evaluation: &str,
    seen: &mut HashSet<String>,
    out: Output,
) {
    let Ok(msgs) = client.evals().messages(evaluation).await else {
        return;
    };
    for m in msgs.iter().filter(|m| seen.insert(m.id.clone())) {
        emit_message(m, out);
    }
}

fn emit_message(m: &EvalMessage, out: Output) {
    if out.is_json() {
        println!(
            "{}",
            serde_json::json!({"error": false, "message": m.message, "level": m.level})
        );
    } else {
        println!("{}", logfmt::eval_message_line(&m.level, &m.message));
    }
}
