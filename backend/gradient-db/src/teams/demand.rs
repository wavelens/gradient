/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::workers::projects_granted_with_workers;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::sea_query::Query;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct TeamDemand {
    pub builds: Vec<UnmetRequirement>,
    pub network: u32,
    pub secrets: u32,
    pub eval: u32,
}

pub async fn team_demand(
    db: &impl ConnectionTrait,
    team: TeamId,
    projects: Option<&[ProjectId]>,
) -> Result<TeamDemand, DbErr> {
    let granting: Vec<ProjectId> = projects_granted_with_workers(db, team)
        .await?
        .into_iter()
        .filter(|p| projects.is_none_or(|only| only.contains(p)))
        .collect();
    if granting.is_empty() {
        return Ok(TeamDemand::default());
    }

    let tasks = Query::select()
        .column(CTask::Id)
        .from(ETask::default())
        .and_where(CTask::Project.is_in(granting))
        .to_owned();
    let reasons = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .filter(CEvaluation::Task.in_subquery(tasks))
        .all(db)
        .await?
        .into_iter()
        .filter_map(|e| e.waiting_reason.as_ref().and_then(WaitingReason::from_json));
    Ok(aggregate(reasons))
}

fn aggregate(reasons: impl IntoIterator<Item = WaitingReason>) -> TeamDemand {
    let mut demand = TeamDemand::default();
    let mut builds: BTreeMap<(String, Vec<String>), u32> = BTreeMap::new();
    for reason in reasons {
        match reason {
            WaitingReason::Workers { unmet, .. } => {
                for requirement in unmet {
                    let mut features = requirement.required_features;
                    features.sort();
                    *builds
                        .entry((requirement.architecture, features))
                        .or_default() += requirement.build_count;
                }
            }
            WaitingReason::EvalWorkers { capability, .. } => match capability {
                EvalCapability::Network => demand.network += 1,
                EvalCapability::Secrets => demand.secrets += 1,
                EvalCapability::Eval => demand.eval += 1,
                EvalCapability::Exec => {}
            },
            _ => {}
        }
    }
    demand.builds = builds
        .into_iter()
        .map(
            |((architecture, required_features), build_count)| UnmetRequirement {
                architecture,
                required_features,
                build_count,
            },
        )
        .collect();
    demand
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, mock_db};

    fn grant(project: ProjectId) -> MTeamProject {
        MTeamProject {
            project,
            includes_workers: true,
            ..Default::default()
        }
    }

    fn waiting(reason: WaitingReason) -> MEvaluation {
        MEvaluation {
            status: EvaluationStatus::Waiting,
            waiting_reason: Some(reason.to_json()),
            ..Default::default()
        }
    }

    fn unmet(architecture: &str, features: &[&str], build_count: u32) -> UnmetRequirement {
        UnmetRequirement {
            architecture: architecture.into(),
            required_features: features.iter().map(|f| f.to_string()).collect(),
            build_count,
        }
    }

    #[tokio::test]
    async fn merges_unmet_builds_by_system_and_features() {
        let project = ProjectId::now_v7();
        let db = mock_db()
            .append_query_results([vec![grant(project)]])
            .append_query_results([vec![
                waiting(WaitingReason::workers(
                    vec![unmet("x86_64-linux", &["kvm", "big"], 2)],
                    1,
                    vec![],
                )),
                waiting(WaitingReason::workers(
                    vec![
                        unmet("x86_64-linux", &["big", "kvm"], 3),
                        unmet("aarch64-darwin", &[], 1),
                    ],
                    1,
                    vec![],
                )),
            ]])
            .into_connection();

        let demand = team_demand(&db, TeamId::now_v7(), None)
            .await
            .expect("demand");

        assert_eq!(
            demand.builds,
            vec![
                unmet("aarch64-darwin", &[], 1),
                unmet("x86_64-linux", &["big", "kvm"], 5),
            ]
        );
    }

    #[tokio::test]
    async fn counts_eval_network_and_secrets_gaps() {
        let db = mock_db()
            .append_query_results([vec![grant(ProjectId::now_v7())]])
            .append_query_results([vec![
                waiting(WaitingReason::eval_workers(EvalCapability::Eval, 0)),
                waiting(WaitingReason::eval_workers(EvalCapability::Eval, 2)),
                waiting(WaitingReason::eval_workers(EvalCapability::Network, 0)),
                waiting(WaitingReason::eval_workers(EvalCapability::Secrets, 1)),
                waiting(WaitingReason::Draining),
            ]])
            .into_connection();

        let demand = team_demand(&db, TeamId::now_v7(), None)
            .await
            .expect("demand");

        assert_eq!((demand.eval, demand.network, demand.secrets), (2, 1, 1));
        assert!(demand.builds.is_empty());
    }

    #[tokio::test]
    async fn ignores_projects_without_worker_grant() {
        let db = mock_db()
            .append_query_results([Vec::<MTeamProject>::new()])
            .into_connection();

        let demand = team_demand(&db, TeamId::now_v7(), None)
            .await
            .expect("demand");

        assert_eq!(demand, TeamDemand::default());
        let statements = all_statements(db.into_transaction_log());
        assert!(
            statements
                .iter()
                .all(|s| !s.sql.contains("FROM \"evaluation\""))
        );
        assert!(
            statements
                .iter()
                .any(|s| s.sql.contains("\"includes_workers\" = $"))
        );
    }

    #[tokio::test]
    async fn project_filter_limits_aggregation() {
        let (kept, dropped) = (ProjectId::now_v7(), ProjectId::now_v7());
        let db = mock_db()
            .append_query_results([vec![grant(kept), grant(dropped)]])
            .append_query_results([Vec::<MEvaluation>::new()])
            .into_connection();

        team_demand(&db, TeamId::now_v7(), Some(&[kept]))
            .await
            .expect("demand");

        let evaluations = all_statements(db.into_transaction_log())
            .into_iter()
            .find(|s| s.sql.contains("FROM \"evaluation\""))
            .expect("an evaluation query");
        let values = format!("{:?}", evaluations.values);
        assert!(values.contains(&kept.to_string()));
        assert!(!values.contains(&dropped.to_string()));
    }
}
