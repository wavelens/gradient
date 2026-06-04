/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::rule::{JobContext, ScoreRule, WorkerContext};

#[derive(Debug, Default)]
pub struct FairShareRule;

impl ScoreRule for FairShareRule {
    fn score(&self, _job: &JobContext<'_>, _worker: &WorkerContext<'_>) -> f64 {
        0.0
    }
}
