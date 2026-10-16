/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{EventKind, EventOwner, names};
use crate::ids::{EvaluationId, ProjectId, TeamId};
use crate::waiting_reason::WaitingReason;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerDemand {
    pub team: TeamId,
    pub project: ProjectId,
    pub evaluation: EvaluationId,
    pub reason: WaitingReason,
}

impl EventKind for WorkerDemand {
    const NAME: &'static str = names::WORKER_DEMAND;
    const DURABLE: bool = true;

    fn owner(&self) -> EventOwner {
        EventOwner::team(self.team)
    }

    fn key(&self) -> Option<String> {
        Some(format!(
            "{}:{}:{}",
            names::WORKER_DEMAND,
            self.team,
            self.reason.worker_gap().unwrap_or_default()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waiting_reason::EvalCapability;

    fn wanted(team: TeamId, evaluation: EvaluationId) -> WorkerDemand {
        WorkerDemand {
            team,
            project: ProjectId::now_v7(),
            evaluation,
            reason: WaitingReason::eval_workers(EvalCapability::Eval, 0),
        }
    }

    #[test]
    fn one_pending_delivery_per_team_and_gap() {
        let team = TeamId::now_v7();
        let first = wanted(team, EvaluationId::now_v7());
        let second = wanted(team, EvaluationId::now_v7());
        assert_eq!(first.key(), second.key());
        assert_ne!(
            first.key(),
            wanted(TeamId::now_v7(), first.evaluation).key()
        );
    }
}
