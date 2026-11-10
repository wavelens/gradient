/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::logstream::stream_eval_logs;
use crate::commands::poll_evaluation_until_finished::print_status_changes_and_exit;
use crate::input::{client_from_config, evaluation_uuid_from_reference};
use crate::output::Output;
use connector::task_evaluation_build_reference_parser::TaskOrEvaluationReference;

pub async fn handle_logs(reference: &TaskOrEvaluationReference, exit_code: bool, out: Output) {
    let client = client_from_config(out);
    let evaluation = evaluation_uuid_from_reference(&client, reference, out).await;
    if let Err(e) = stream_eval_logs(&client, &evaluation, out).await {
        out.fail(e);
    }
    if exit_code {
        print_status_changes_and_exit(&client, &evaluation, None, out).await;
    }
}
