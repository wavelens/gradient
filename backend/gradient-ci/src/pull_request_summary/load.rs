/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::render::{Outcome, Package, Summary};
use crate::actions::evaluation_rows::{EvaluationRows, load_project_name};
use crate::context::CiContext;
use anyhow::{Context, Result};
use gradient_db::evaluations::failed_attributes::failed_attributes;
use gradient_db::lookup::by_ids;
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use std::collections::HashMap;

pub(super) async fn load_summary(
    ctx: &CiContext,
    evaluation: &MEvaluation,
    rows: &EvaluationRows,
) -> Result<Summary> {
    let db = &ctx.db.worker_db;
    let evaluation_link = load_project_name(ctx, rows.task.project)
        .await
        .map(|project| {
            format!(
                "{}/project/{}/log/{}",
                ctx.db.config.server.frontend_url, project, evaluation.id
            )
        });

    let entry_points = gradient_db::entry_points::of_evaluation(db, evaluation.id)
        .await
        .context("load entry points")?;
    let derivation_ids: Vec<DerivationId> = entry_points.iter().map(|e| e.derivation).collect();
    let jobs: HashMap<DerivationId, MBuildJob> =
        gradient_db::build_jobs::of_derivations_in_evaluation(db, evaluation.id, &derivation_ids)
            .await
            .context("load build jobs")?
            .into_iter()
            .map(|j| (j.derivation, j))
            .collect();
    let build_ids: Vec<DerivationBuildId> = jobs.values().map(|j| j.derivation_build).collect();
    let builds = by_ids::<EDerivationBuild, _>(db, &build_ids)
        .await
        .context("load shared builds")?;
    let derivations = by_ids::<EDerivation, _>(db, &derivation_ids)
        .await
        .context("load derivations")?;

    let mut summary = Summary {
        task: rows.task.name.clone(),
        commit: vec_to_hex(&rows.commit.hash),
        failed_to_evaluate: failed_attributes(db, evaluation.id)
            .await
            .context("load failed attributes")?
            .into_iter()
            .map(|f| f.attr)
            .collect(),
        ..Default::default()
    };
    for entry_point in entry_points {
        let (Some(job), Some(derivation)) = (
            jobs.get(&entry_point.derivation),
            derivations.get(&entry_point.derivation),
        ) else {
            continue;
        };
        let Some(build) = builds.get(&job.derivation_build) else {
            continue;
        };
        let Some(outcome) = Outcome::of(build.status, build.created_at < evaluation.created_at)
        else {
            continue;
        };

        let log = (outcome == Outcome::Failed)
            .then(|| {
                evaluation_link
                    .as_ref()
                    .map(|l| format!("{l}?build={}", job.id))
            })
            .flatten();
        summary.add(
            &derivation.architecture,
            outcome,
            Package {
                attr: entry_point.eval,
                log,
            },
        );
    }

    summary.link = evaluation_link;
    Ok(summary)
}
