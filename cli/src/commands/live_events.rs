/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::output::{ExitKind, Output};
use connector::{ConnectorError, LiveUpdateEvent};
use futures::{Stream, StreamExt, pin_mut};
use std::future::Future;
use std::time::Duration;

const FOLLOWED_EVALUATION_OR_BUILD_FINISH_RECHECK_INTERVAL: Duration = Duration::from_secs(15);

/// Finish checks run right after socket open, after each status event and on a timer. The server can drop events for clients that lag behind.
pub async fn print_live_events_until_finished<F, Fut>(
    events: impl Stream<Item = Result<LiveUpdateEvent, ConnectorError>>,
    finished: F,
    out: Output,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<bool, ConnectorError>>,
{
    pin_mut!(events);
    let mut recheck = tokio::time::interval(FOLLOWED_EVALUATION_OR_BUILD_FINISH_RECHECK_INTERVAL);
    loop {
        tokio::select! {
            _ = recheck.tick() => {}
            event = events.next() => match event {
                None => out.err(
                    ExitKind::Api,
                    "The server closed the live connection before the evaluation or build completed.",
                ),
                Some(Err(e)) => out.fail(e),
                Some(Ok(event)) => {
                    out.ok(&event);
                    out.human(format!("{} {} {}", event.at, event.event, event.content));
                    if event.event.ends_with(".progress") || event.event.ends_with(".activity") {
                        continue;
                    }
                }
            },
        }
        match finished().await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => out.fail(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn build_successful_live_update_event_with_name(
        name: &str,
    ) -> Result<LiveUpdateEvent, ConnectorError> {
        Ok(LiveUpdateEvent {
            event: name.to_owned(),
            at: "2026-11-09T00:00:00Z".to_owned(),
            content: serde_json::json!({}),
        })
    }

    #[tokio::test]
    async fn live_event_follow_stops_when_finish_check_done_after_start_or_status_event_but_not_after_progress_event()
     {
        let events = futures::stream::iter([
            build_successful_live_update_event_with_name("build.progress"),
            build_successful_live_update_event_with_name("build.status_changed"),
        ])
        .chain(futures::stream::pending());
        let checks = AtomicUsize::new(0);
        print_live_events_until_finished(
            events,
            || async { Ok(checks.fetch_add(1, Ordering::SeqCst) == 1) },
            Output::new(true),
        )
        .await;
        assert_eq!(checks.load(Ordering::SeqCst), 2);
    }
}
