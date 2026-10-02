/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::logstream::stream_eval_logs;
use crate::input::client_from_config;
use crate::output::Output;

pub async fn handle_logs(evaluation: &str, out: Output) {
    let client = client_from_config(out);
    stream_eval_logs(&client, evaluation, out).await;
}
