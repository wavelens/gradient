/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod analysis;

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::evaluations::failed_attributes::{FailedAttribute, failed_attributes};
use gradient_entity::build::BuildStatus;
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

pub use self::analysis::PackageCounts;
use self::analysis::{Baseline, EvaluationGraph, Failure, Package};
use super::EvalAccessContext;

const IS_IN_CHUNK: usize = 10_000;
const FAILURES_SHOWN: usize = 100;
const BLOCKED_SHOWN: usize = 20;
const FIXED_SHOWN: usize = 100;

#[derive(Serialize, Debug)]
pub struct EvaluationFailureSummary {
    pub compared_with: Option<ComparedEvaluation>,
    pub packages: PackageCounts,
    pub failures: Vec<FailedBuildSummary>,
    pub failures_total: usize,
    pub failed_attributes: Vec<FailedAttributeReport>,
    pub fixed: Vec<String>,
    pub fixed_total: usize,
}

#[derive(Serialize, Debug)]
pub struct ComparedEvaluation {
    pub id: EvaluationId,
    pub commit: String,
}

#[derive(Serialize, Debug)]
pub struct FailedBuildSummary {
    pub build_id: BuildJobId,
    pub name: String,
    pub derivation_path: String,
    pub architecture: gradient_entity::server::Architecture,
    pub status: BuildStatus,
    pub attributes: Vec<String>,
    pub blocked: Vec<String>,
    pub blocked_total: usize,
    pub newly_failing: bool,
}

#[derive(Serialize, Debug)]
pub struct FailedAttributeReport {
    pub attr: String,
    pub message: String,
    pub newly_failing: bool,
}

struct LoadedEvaluation {
    graph: EvaluationGraph,
    build_jobs: HashMap<DerivationId, BuildJobId>,
    failed_attributes: Vec<FailedAttribute>,
}

pub async fn get_evaluation_summary(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<EvaluationFailureSummary>>> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;
    let db = &state.web_db;
    let loaded = LoadedEvaluation::load(db, ctx.evaluation.id).await?;
    let compared = match ctx.evaluation.previous {
        Some(previous) => compared_evaluation(db, previous).await?,
        None => None,
    };
    let baseline = compared.as_ref().map(|(_, baseline)| baseline);
    let failures = analysis::failures(&loaded.graph, baseline);
    let fixed = baseline.map_or_else(Vec::new, |before| analysis::fixed(&loaded.graph, before));

    Ok(ok_json(EvaluationFailureSummary {
        packages: analysis::package_counts(&loaded.graph),
        failures_total: failures.len(),
        failures: failed_builds(db, &loaded, failures).await?,
        failed_attributes: failed_attribute_reports(loaded.failed_attributes, baseline),
        fixed_total: fixed.len(),
        fixed: fixed.into_iter().take(FIXED_SHOWN).collect(),
        compared_with: compared.map(|(evaluation, _)| evaluation),
    }))
}

impl LoadedEvaluation {
    async fn load<C: ConnectionTrait>(db: &C, evaluation: EvaluationId) -> Result<Self, DbErr> {
        let jobs = EBuildJob::find()
            .filter(CBuildJob::Evaluation.eq(evaluation))
            .all(db)
            .await?;
        let shared = shared_build_statuses(db, &jobs).await?;
        let statuses: HashMap<DerivationId, BuildStatus> = jobs
            .iter()
            .filter_map(|j| Some((j.derivation, shared.get(&j.derivation_build)?.for_api())))
            .collect();
        let packages = EEntryPoint::find()
            .filter(CEntryPoint::Evaluation.eq(evaluation))
            .all(db)
            .await?
            .into_iter()
            .filter(|e| statuses.contains_key(&e.derivation))
            .map(|e| Package {
                attr: e.eval,
                derivation: e.derivation,
            })
            .collect();

        Ok(Self {
            graph: EvaluationGraph {
                statuses,
                edges: gradient_db::graph::layers::eval_dependency_edges(db, evaluation).await?,
                packages,
            },
            build_jobs: jobs.iter().map(|j| (j.derivation, j.id)).collect(),
            failed_attributes: failed_attributes(db, evaluation).await?,
        })
    }
}

async fn shared_build_statuses<C: ConnectionTrait>(
    db: &C,
    jobs: &[MBuildJob],
) -> Result<HashMap<DerivationBuildId, BuildStatus>, DbErr> {
    let ids: Vec<DerivationBuildId> = jobs.iter().map(|j| j.derivation_build).collect();
    let mut statuses = HashMap::new();
    for chunk in ids.chunks(IS_IN_CHUNK) {
        let builds = EDerivationBuild::find()
            .filter(CDerivationBuild::Id.is_in(chunk.to_vec()))
            .all(db)
            .await?;
        statuses.extend(builds.into_iter().map(|b| (b.id, b.status)));
    }

    Ok(statuses)
}

async fn compared_evaluation<C: ConnectionTrait>(
    db: &C,
    previous: EvaluationId,
) -> Result<Option<(ComparedEvaluation, Baseline)>, DbErr> {
    let Some(evaluation) = EEvaluation::find_by_id(previous).one(db).await? else {
        return Ok(None);
    };
    let commit = ECommit::find_by_id(evaluation.commit)
        .one(db)
        .await?
        .map(|c| vec_to_hex(&c.hash))
        .unwrap_or_default();
    let loaded = LoadedEvaluation::load(db, previous).await?;
    let failed = loaded
        .failed_attributes
        .into_iter()
        .map(|f| f.attr)
        .collect();

    Ok(Some((
        ComparedEvaluation {
            id: previous,
            commit,
        },
        Baseline::of(&loaded.graph, failed),
    )))
}

async fn failed_builds<C: ConnectionTrait>(
    db: &C,
    loaded: &LoadedEvaluation,
    failures: Vec<Failure>,
) -> Result<Vec<FailedBuildSummary>, DbErr> {
    let shown: Vec<Failure> = failures.into_iter().take(FAILURES_SHOWN).collect();
    let derivations: HashMap<DerivationId, MDerivation> = EDerivation::find()
        .filter(CDerivation::Id.is_in(shown.iter().map(|f| f.derivation)))
        .all(db)
        .await?
        .into_iter()
        .map(|d| (d.id, d))
        .collect();

    Ok(shown
        .into_iter()
        .filter_map(|failure| {
            let derivation = derivations.get(&failure.derivation)?;

            Some(FailedBuildSummary {
                build_id: *loaded.build_jobs.get(&failure.derivation)?,
                name: derivation.name.clone(),
                derivation_path: derivation.drv_path(),
                architecture: derivation.architecture.clone(),
                status: failure.status,
                attributes: failure.attributes,
                blocked_total: failure.blocked.len(),
                blocked: failure.blocked.into_iter().take(BLOCKED_SHOWN).collect(),
                newly_failing: failure.newly_failing,
            })
        })
        .collect())
}

fn failed_attribute_reports(
    failed: Vec<FailedAttribute>,
    baseline: Option<&Baseline>,
) -> Vec<FailedAttributeReport> {
    failed
        .into_iter()
        .map(|f| FailedAttributeReport {
            newly_failing: baseline.is_some_and(|before| !before.broken.contains(&f.attr)),
            attr: f.attr,
            message: f.message,
        })
        .collect()
}
