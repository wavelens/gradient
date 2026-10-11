/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gradient_graph::RamShortage;
use gradient_types::DerivationBuildId;

/// A server restart or a worker redeploy must not fail large builds. The grace gives the workers
/// able to hold them the time to connect again.
pub(crate) const RAM_SHORTAGE_GRACE: Duration = Duration::from_secs(300);

#[derive(Debug, Default)]
pub(crate) struct RamShortages {
    since: HashMap<DerivationBuildId, Instant>,
}

impl RamShortages {
    pub(crate) fn lasting(
        &mut self,
        current: Vec<RamShortage>,
        now: Instant,
        grace: Duration,
    ) -> Vec<RamShortage> {
        let short: HashSet<DerivationBuildId> = current.iter().map(|s| s.shared_build).collect();
        self.since.retain(|build, _| short.contains(build));

        current
            .into_iter()
            .filter(|shortage| {
                let since = *self.since.entry(shortage.shared_build).or_insert(now);
                now.duration_since(since) >= grace
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::EvaluationId;

    const GRACE: Duration = Duration::from_secs(300);

    fn shortage() -> RamShortage {
        RamShortage {
            shared_build: DerivationBuildId::now_v7(),
            evaluation: EvaluationId::now_v7(),
            needed_mb: 124_540,
            largest_available_mb: 102_754,
        }
    }

    #[test]
    fn a_shortage_is_reported_once_it_lasted_the_whole_grace() {
        let mut shortages = RamShortages::default();
        let build = shortage();
        let start = Instant::now();

        assert!(shortages.lasting(vec![build], start, GRACE).is_empty());
        assert!(
            shortages
                .lasting(vec![build], start + GRACE - Duration::from_secs(1), GRACE)
                .is_empty()
        );
        assert_eq!(
            shortages.lasting(vec![build], start + GRACE, GRACE),
            vec![build]
        );
    }

    #[test]
    fn a_build_that_fit_a_worker_in_between_waits_out_a_new_grace() {
        let mut shortages = RamShortages::default();
        let build = shortage();
        let start = Instant::now();

        shortages.lasting(vec![build], start, GRACE);
        shortages.lasting(Vec::new(), start + GRACE / 2, GRACE);

        assert!(
            shortages
                .lasting(vec![build], start + GRACE, GRACE)
                .is_empty()
        );
        assert_eq!(
            shortages.lasting(vec![build], start + GRACE + GRACE, GRACE),
            vec![build]
        );
    }
}
