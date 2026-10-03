/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::convert::Infallible;

use gradient_wire::messages::EVAL_PROGRESS_INTERVAL;
use gradient_wire::traits::EvalProgressSink;
use gradient_wire::types::EvalProgress;
use tokio::time::MissedTickBehavior;

#[derive(Default)]
pub(crate) struct ChangeReporter {
    last: Option<EvalProgress>,
}

impl ChangeReporter {
    pub(crate) async fn run(
        &mut self,
        sink: &dyn EvalProgressSink,
        snapshot: impl Fn() -> Option<EvalProgress>,
    ) -> Infallible {
        let mut ticks = tokio::time::interval(EVAL_PROGRESS_INTERVAL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            self.flush(sink, &snapshot).await;
        }
    }

    pub(crate) async fn flush(
        &mut self,
        sink: &dyn EvalProgressSink,
        snapshot: &impl Fn() -> Option<EvalProgress>,
    ) {
        let Some(now) = snapshot() else { return };
        if self.last.as_ref() != Some(&now) {
            sink.report(now.clone()).await;
            self.last = Some(now);
        }
    }
}

pub(crate) fn thunk_progress(thunks: u64) -> Option<EvalProgress> {
    (thunks > 0).then_some(EvalProgress::Evaluating { thunks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_util::sync::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct Sent(Mutex<Vec<EvalProgress>>);

    #[async_trait::async_trait]
    impl EvalProgressSink for Sent {
        async fn report(&self, progress: EvalProgress) {
            self.0.lock().push(progress);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn reports_at_most_once_per_second_and_only_on_change() {
        let sent = Sent::default();
        let thunks = Mutex::new(1u64);
        let snapshot = || {
            Some(EvalProgress::Evaluating {
                thunks: *thunks.lock(),
            })
        };
        let mut reporter = ChangeReporter::default();
        let run = reporter.run(&sent, snapshot);
        tokio::pin!(run);

        tokio::select! { _ = &mut run => {}, () = tokio::time::sleep(Duration::from_millis(1500)) => {} }
        *thunks.lock() = 2;
        tokio::select! { _ = &mut run => {}, () = tokio::time::sleep(Duration::from_millis(300)) => {} }
        assert_eq!(
            sent.0.lock().len(),
            1,
            "unchanged at 1 s, change not due until 2 s"
        );
        tokio::select! { _ = &mut run => {}, () = tokio::time::sleep(Duration::from_millis(400)) => {} }
        assert_eq!(
            *sent.0.lock(),
            vec![
                EvalProgress::Evaluating { thunks: 1 },
                EvalProgress::Evaluating { thunks: 2 },
            ]
        );
    }

    #[tokio::test]
    async fn zero_thunks_send_nothing() {
        let sent = Sent::default();
        let mut reporter = ChangeReporter::default();
        reporter.flush(&sent, &|| thunk_progress(0)).await;
        assert!(sent.0.lock().is_empty());
    }
}
