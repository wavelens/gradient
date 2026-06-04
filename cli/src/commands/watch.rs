/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::client_from_config;
use crate::output::{ExitKind, Output, to_exit_kind};
use crate::tui::watch::{Dashboard, WatchEvent, eval_is_terminal};
use connector::Client;
use futures::{StreamExt, pin_mut};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

pub async fn handle_watch(eval_id: &str, out: Output) {
    let client = client_from_config(out);

    if client.evals().get(eval_id).await.is_err() {
        out.err(ExitKind::Api, format!("Unknown evaluation: {eval_id}"));
    }

    if out.is_json() {
        return stream_plain(&client, eval_id, out).await;
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<WatchEvent>();
    tokio::spawn(poll_metadata(client.clone(), eval_id.to_string(), tx.clone()));
    tokio::spawn(stream_logs(client.clone(), eval_id.to_string(), tx));

    crate::tui::run(Dashboard::new(eval_id.to_string(), rx))
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("tui error: {e}")));
}

async fn poll_metadata(client: Client, eval_id: String, tx: UnboundedSender<WatchEvent>) {
    loop {
        let evals = client.evals();
        let terminal = match evals.get(&eval_id).await {
            Ok(eval) => {
                let terminal = eval_is_terminal(&eval.status);
                let _ = tx.send(WatchEvent::Eval(eval));
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

async fn stream_plain(client: &Client, eval_id: &str, out: Output) {
    let evals = client.evals();
    let stream = match evals.stream_builds(eval_id).await {
        Ok(s) => s,
        Err(e) => out.err(to_exit_kind(&e), e),
    };
    pin_mut!(stream);
    while let Some(item) = stream.next().await {
        match item {
            Ok(line) => println!("{}", serde_json::json!({"error": false, "message": line})),
            Err(e) => out.err(to_exit_kind(&e), e),
        }
    }
}
