/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::poll_evaluation_until_finished::print_status_changes_and_exit;
use crate::error::CliError;
use crate::input::{client_from_config, evaluation_uuid_from_reference, is_interactive};
use crate::output::{ExitKind, Output};
use crate::tui::watch::{Dashboard, WatchEvent, eval_is_terminal};
use connector::Client;
use connector::task_evaluation_build_reference_parser::TaskOrEvaluationReference;
use futures::{StreamExt, pin_mut};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

pub async fn handle_watch(reference: &TaskOrEvaluationReference, out: Output) {
    let client = client_from_config(out);
    let eval_id = evaluation_uuid_from_reference(&client, reference, out).await;

    if client.evals().get(&eval_id).await.is_err() {
        out.err(ExitKind::Api, format!("Unknown evaluation: {eval_id}"));
    }

    if !is_interactive(out) {
        print_status_changes_and_exit(&client, &eval_id, None, out).await;
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<WatchEvent>();
    tokio::spawn(poll_metadata(client.clone(), eval_id.clone(), tx.clone()));
    tokio::spawn(stream_logs(client.clone(), eval_id.clone(), tx));

    crate::tui::run(Dashboard::new(eval_id, rx))
        .unwrap_or_else(|e| out.fail(CliError::TerminalUi(e)));
}

async fn poll_metadata(client: Client, eval_id: String, tx: UnboundedSender<WatchEvent>) {
    loop {
        let evals = client.evals();
        let terminal = match evals.get(&eval_id).await {
            Ok(eval) => {
                let terminal = eval_is_terminal(eval.status);
                let _ = tx.send(WatchEvent::Eval(Box::new(eval)));
                terminal
            }
            Err(_) => false,
        };
        if let Ok(builds) = evals.builds(&eval_id).await {
            let _ = tx.send(WatchEvent::Builds(builds.builds));
        }
        if let Ok(messages) = evals.messages(&eval_id).await {
            for m in messages {
                let _ = tx.send(WatchEvent::Message(m));
            }
        }
        if terminal || tx.is_closed() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn stream_logs(client: Client, eval_id: String, tx: UnboundedSender<WatchEvent>) {
    let evals = client.evals();
    let stream = match evals.stream_builds(&eval_id).await {
        Ok(s) => s,
        Err(_) => return,
    };
    pin_mut!(stream);
    while let Some(Ok(line)) = stream.next().await {
        if tx.send(WatchEvent::Log(line)).is_err() {
            break;
        }
    }
}
