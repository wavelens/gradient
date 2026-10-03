/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Instant;

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::EvaluationProgress;
use gradient_types::ids::EvaluationId;
use gradient_util::latest::Latest;

pub(crate) fn live_progress(
    store: &Latest<EvaluationId, EvaluationProgress>,
    evaluation: EvaluationId,
    status: EvaluationStatus,
    now: Instant,
) -> Option<EvaluationProgress> {
    use EvaluationStatus::{EvaluatingDerivation, EvaluatingFlake, Fetching};
    matches!(status, Fetching | EvaluatingFlake | EvaluatingDerivation)
        .then(|| store.get(&evaluation, now))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::evaluation::EvaluationStatus as S;
    use std::time::Duration;

    #[test]
    fn only_a_running_evaluation_shows_its_progress() {
        let store = Latest::new(Duration::from_secs(60));
        let id = EvaluationId::nil();
        let now = Instant::now();
        store.set(id, EvaluationProgress::Evaluating { thunks: 3 }, now);
        for status in [S::Fetching, S::EvaluatingFlake, S::EvaluatingDerivation] {
            assert!(
                live_progress(&store, id, status, now).is_some(),
                "{status:?}"
            );
        }
        for status in [S::Queued, S::Building, S::Completed, S::Failed] {
            assert!(
                live_progress(&store, id, status, now).is_none(),
                "{status:?}"
            );
        }
    }
}
