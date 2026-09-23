/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! User-requested prioritization (#530): the graph actor flags the rows, then
//! the jobs already tracked are lifted in place.

use gradient_graph::Transition;
use gradient_types::*;

use crate::Scheduler;
use crate::actor::SchedulerMsg;

impl Scheduler {
    pub async fn prioritize_evaluation(&self, evaluation: EvaluationId) -> anyhow::Result<()> {
        self.prioritize(
            Some(evaluation),
            Transition::PrioritizeEvaluation { evaluation },
        )
        .await
    }

    pub async fn prioritize_build(&self, anchor: DerivationBuildId) -> anyhow::Result<()> {
        self.prioritize(None, Transition::PrioritizeBuild { anchor })
            .await
    }

    async fn prioritize(
        &self,
        evaluation: Option<EvaluationId>,
        transition: Transition,
    ) -> anyhow::Result<()> {
        let anchors = self
            .state
            .graph
            .transition(transition)
            .await?
            .prioritized_anchors;
        self.call(|reply| SchedulerMsg::Prioritize {
            evaluation,
            anchors,
            reply,
        })
        .await?;
        Ok(())
    }
}
