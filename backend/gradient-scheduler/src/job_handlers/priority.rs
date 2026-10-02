/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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

    pub async fn prioritize_build(&self, shared_build: DerivationBuildId) -> anyhow::Result<()> {
        self.prioritize(None, Transition::PrioritizeBuild { shared_build })
            .await
    }

    async fn prioritize(
        &self,
        evaluation: Option<EvaluationId>,
        transition: Transition,
    ) -> anyhow::Result<()> {
        let shared_builds = self
            .state
            .graph
            .transition(transition)
            .await?
            .prioritized_shared_builds;
        self.call(|reply| SchedulerMsg::Prioritize {
            evaluation,
            shared_builds,
            reply,
        })
        .await?;
        Ok(())
    }
}
