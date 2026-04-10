/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod ci;
mod evaluation;
mod queue;

pub use self::evaluation::schedule_evaluation;

use gradient_core::types::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time;
use self::queue::get_next_evaluation;

pub async fn schedule_evaluation_loop(state: Arc<ServerState>) {
    let _guard = if state.cli.report_errors {
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        None
    };

    let mut current_schedules = vec![];
    let mut interval = time::interval(Duration::from_secs(5));

    loop {
        let mut added_schedule = false;
        current_schedules.retain(|schedule: &JoinHandle<()>| !schedule.is_finished());

        // TODO: look at tokio semaphore
        while current_schedules.len() < state.cli.max_concurrent_evaluations {
            let evaluation = get_next_evaluation(Arc::clone(&state)).await;
            let schedule = tokio::spawn(schedule_evaluation(Arc::clone(&state), evaluation));
            current_schedules.push(schedule);
            added_schedule = true;
        }

        if !added_schedule {
            interval.tick().await;
        }
    }
}
