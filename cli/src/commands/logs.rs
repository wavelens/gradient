/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::logstream::stream_eval_logs;
use crate::input::client_from_config;
use crate::output::Output;

/// Stream the full log of an evaluation: every build's stored log followed by
/// the active ones, interleaved with eval-level messages. Works both live and
/// on a finished evaluation (the server replays the stored logs, then ends).
pub async fn handle_logs(evaluation: &str, out: Output) {
    let client = client_from_config(out);
    stream_eval_logs(&client, evaluation, out).await;
}
