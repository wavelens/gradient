/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::output::{ExitKind, Output};
use crate::tui::watch::eval_is_terminal;
use connector::api::EvaluationResponse;
use connector::{Client, ConnectorError};
use std::time::Duration;
use tokio::time::Instant;

const WATCH_POLL: Duration = Duration::from_secs(5);

fn is_transient(e: &ConnectorError) -> bool {
    match e {
        ConnectorError::Transport(_) => true,
        ConnectorError::Api { status, .. } => status.is_server_error(),
        _ => false,
    }
}

pub async fn poll_evaluation_until_terminal(
    client: &Client,
    evaluation: &str,
    deadline: Option<Instant>,
    mut on_status_change: impl FnMut(&EvaluationResponse),
) -> Result<EvaluationResponse, ConnectorError> {
    let mut last_read: Option<EvaluationResponse> = None;
    loop {
        let polled = client.evals().get(evaluation).await;
        match &polled {
            Ok(eval) => {
                if last_read
                    .as_ref()
                    .is_none_or(|last| last.status != eval.status)
                {
                    on_status_change(eval);
                }
                if eval_is_terminal(eval.status) {
                    return polled;
                }
            }
            Err(e) if !is_transient(e) => return polled,
            Err(_) => {}
        }

        let left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        if left.is_some_and(|left| left.is_zero()) {
            return polled.or_else(|e| last_read.ok_or(e));
        }
        if let Ok(eval) = polled {
            last_read = Some(eval);
        }
        tokio::time::sleep(left.map_or(WATCH_POLL, |left| left.min(WATCH_POLL))).await;
    }
}

pub async fn print_status_changes_and_exit(
    client: &Client,
    evaluation: &str,
    timeout: Option<Duration>,
    out: Output,
) -> ! {
    let eval = poll_evaluation_until_terminal(
        client,
        evaluation,
        timeout.map(|timeout| Instant::now() + timeout),
        |eval| {
            out.ok(&serde_json::json!({
                "evaluation": eval.id,
                "status": eval.status,
                "error": eval.error,
            }));
            out.human(format!("Evaluation {}: {}", eval.id, eval.status));
        },
    )
    .await
    .unwrap_or_else(|e| out.fail(e));

    match ExitKind::exit_kind_from_evaluation_status(eval.status) {
        Some(kind) => {
            if let Some(error) = eval.error.as_deref().filter(|_| !out.is_json()) {
                out.progress(error);
            }
            kind.exit_with_code_without_printing()
        }
        None => out.err(
            ExitKind::EvaluationTimeout,
            format!(
                "Evaluation {evaluation} still in {} after {} seconds.",
                eval.status,
                timeout.unwrap_or_default().as_secs()
            ),
        ),
    }
}
