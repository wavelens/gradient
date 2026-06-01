/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::client_from_config;
use crate::output::{ExitKind, Output, to_exit_kind};
use futures::{StreamExt, pin_mut};

pub async fn handle_log(id: &str, interactive: bool, out: Output) {
    let client = client_from_config(out);
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
