/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use futures::{StreamExt as _, TryStreamExt as _};
use gradient_db::lookup::by_id;
use gradient_db::status::{EvaluationFailure, update_evaluation_status_with_error};
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::EntityTrait;
use tracing::{debug, warn};

use gradient_graph::derivation_closure_input_addressed_output_verification::{
    DrvSource, load_stored_derivation_hash_modulo_text_by_store_path_for_chunk,
    read_transitive_input_derivations_checking_input_addressed_output_paths,
};
use gradient_graph::read_derivation_from_trusted_nar::{
    StorePathVerifiedDerivationRecord, load_verified_trusted_derivation_record,
    map_verified_derivation_records_by_store_path,
};
use gradient_graph::writer::derivation_save_failure_user_message;
use gradient_graph::{EvaluationBatchDerivation, RecordBatch};
use gradient_types::*;
use gradient_wire::types::DiscoveredDerivation;

const MAX_CONCURRENT_VERIFIED_DERIVATION_FILE_READS: usize = 16;

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::jobs::{PendingEvalJob, PendingJob};

#[derive(Debug, thiserror::Error)]
#[error("The graph writer did not save the report batch for evaluation {evaluation_id}.")]
pub struct GraphWriterDidNotSaveReportBatchError {
    pub evaluation_id: EvaluationId,
    #[source]
    pub graph_writer_error: anyhow::Error,
}

impl Scheduler {
    pub async fn handle_eval_status_update(
        &self,
        job_id: &str,
        new_status: gradient_entity::evaluation::EvaluationStatus,
    ) {
        let Some(PendingJob::Eval(PendingEvalJob {
            evaluation_id,
            continuation: None,
            ..
        })) = self.active_job(job_id).await
        else {
            return;
        };
        match by_id::<EEvaluation, _>(&self.db.worker_db, evaluation_id).await {
            Ok(Some(eval)) => {
                if let Err(e) =
                    gradient_db::status::update_evaluation_status(&self.db, eval, new_status).await
                {
                    warn!(error = %e, %evaluation_id, "failed to update the evaluation status");
                }
            }
            Ok(None) => warn!(%evaluation_id, "evaluation not found for status update"),
            Err(e) => {
                warn!(error = %e, %evaluation_id, "failed to fetch evaluation for status update")
            }
        }
    }

    pub async fn persist_fetch_result(&self, job_id: &str, flake_source: Option<String>) {
        use sea_orm::ActiveModelTrait;
        use sea_orm::Set;

        let Some(PendingJob::Eval(PendingEvalJob {
            evaluation_id,
            task_id,
            continuation: None,
            ..
        })) = self.active_job(job_id).await
        else {
            return;
        };
        if let Some(task) = task_id
            && let Err(e) =
                gradient_db::tasks::record_fetch(&self.db.worker_db, task, now(), None).await
        {
            warn!(error = %e, %task, "failed to record repository fetch");
        }
        let Some(path) = flake_source else { return };
        let am = gradient_entity::evaluation::ActiveModel {
            id: Set(evaluation_id),
            flake_source: Set(Some(path)),
            ..Default::default()
        };
        if let Err(e) = am.update(&self.db.worker_db).await {
            warn!(error = %e, %evaluation_id, "failed to persist flake_source");
        }
    }

    pub async fn persist_input_update_result(
        &self,
        job_id: &str,
        candidate_lock: String,
        bumped: Vec<gradient_wire::types::BumpedInputWire>,
    ) {
        use gradient_entity::evaluation_input_update as eiu;
        use sea_orm::{
            ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
        };

        let Some(PendingJob::Eval(j)) = self.active_job(job_id).await else {
            return;
        };
        let evaluation_id = j.evaluation_id;

        let bumped_json = serde_json::json!(
            bumped
                .iter()
                .map(|b| serde_json::json!({
                    "name": b.name,
                    "old_rev": b.old_rev,
                    "new_rev": b.new_rev,
                }))
                .collect::<Vec<_>>()
        );

        let sidecar = match eiu::Entity::find()
            .filter(eiu::Column::Evaluation.eq(evaluation_id))
            .one(&self.db.worker_db)
            .await
        {
            Ok(Some(s)) => s,
            Ok(None) => {
                warn!(%evaluation_id, "input_update sidecar missing for result");
                return;
            }
            Err(e) => {
                warn!(error = %e, %evaluation_id, "loading input_update sidecar");
                return;
            }
        };

        let mut am = sidecar.into_active_model();
        am.candidate_lock = Set(Some(candidate_lock));
        am.bumped_inputs = Set(Some(bumped_json));
        am.updated_at = Set(gradient_types::now());
        if let Err(e) = am.update(&self.db.worker_db).await {
            warn!(error = %e, %evaluation_id, "failed to persist input_update result");
        }
    }

    pub async fn persist_input_update_expansion(&self, job_id: &str, matched: Vec<String>) {
        use sea_orm::{ColumnTrait, QueryFilter};

        let Some(PendingJob::Eval(j)) = self.active_job(job_id).await else {
            return;
        };
        let evaluation_id = j.evaluation_id;
        let db = &self.db.worker_db;

        let eval = match by_id::<EEvaluation, _>(db, evaluation_id).await {
            Ok(Some(e)) => e,
            _ => {
                warn!(%evaluation_id, "input_update expansion: evaluation missing");
                return;
            }
        };
        let Some(task_id) = eval.task else {
            return;
        };
        let sidecar = match gradient_entity::evaluation_input_update::Entity::find()
            .filter(gradient_entity::evaluation_input_update::Column::Evaluation.eq(evaluation_id))
            .one(db)
            .await
        {
            Ok(Some(s)) => s,
            _ => {
                warn!(%evaluation_id, "input_update expansion: sidecar missing");
                return;
            }
        };
        let task = match by_id::<ETask, _>(db, task_id).await {
            Ok(Some(p)) => p,
            _ => {
                warn!(%task_id, "input_update expansion: task missing");
                return;
            }
        };

        match gradient_ci::trigger::fan_out_expansion(
            db,
            &task,
            sidecar.base_commit,
            matched,
            eval.trigger,
        )
        .await
        {
            Ok(created) => {
                debug!(%evaluation_id, count = created.len(), "fanned out input_update expansion")
            }
            Err(e) => warn!(error = %e, %evaluation_id, "input_update fan-out failed"),
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(%job_id, derivations = derivations.len()))]
    pub async fn handle_eval_result(
        &self,
        job_id: &str,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        let job = match self.active_job(job_id).await {
            Some(PendingJob::Eval(j)) => j,
            Some(_) => anyhow::bail!("job {} is not an eval job", job_id),
            None => {
                warn!(%job_id, "eval result for unknown job - ignoring");
                return Ok(());
            }
        };

        let project = job.project_id;
        let mapper = match self.db.project_mapper(project).await {
            Ok(mapper) => mapper,
            Err(error) => {
                warn!(%project, %error, "The scheduler can't map the store path for a project. The evaluation can't finish.");
                self.mark_evaluation_failed_with_message(
                    job.evaluation_id,
                    "Evaluation can't continue because the server can't map this project's store paths.",
                )
                .await;
                return Ok(());
            }
        };
        let verified = futures::stream::iter(derivations)
            .map(|reported| self.load_trusted_evaluation_batch_derivation(project, reported))
            .buffered(MAX_CONCURRENT_VERIFIED_DERIVATION_FILE_READS)
            .try_collect::<Vec<_>>()
            .await;
        let mut derivations = match verified {
            Ok(derivations) => derivations,
            Err(drv_path) => {
                self.mark_evaluation_failed_with_message(
                    job.evaluation_id,
                    &mapper.rewrite_stored_store_paths_in_text_to_outbound(&format!(
                        "derivation file {drv_path} was not uploaded or does not match its path"
                    )),
                )
                .await;
                return Ok(());
            }
        };

        let reported = match map_verified_derivation_records_by_store_path(
            derivations
                .iter()
                .map(|batch_entry| &batch_entry.store_path_verified_derivation_record),
        ) {
            Ok(reported) => reported,
            Err(error) => {
                self.mark_evaluation_failed_with_message(
                    job.evaluation_id,
                    &mapper.rewrite_stored_store_paths_in_text_to_outbound(&format!("{error:#}")),
                )
                .await;
                return Ok(());
            }
        };
        let source = ReportedInputDerivationFileAndHashModuloSource {
            db: &self.db,
            reported,
        };
        let roots: Vec<String> = source.reported.keys().cloned().collect();
        let derivation_closure_check_read_derivations_and_verified_hash_modulos_for_equal_client_and_stored_paths = match read_transitive_input_derivations_checking_input_addressed_output_paths(
            &source, &roots,
        )
        .await
        {
            Ok(derivation_closure_check_read_derivations_and_verified_hash_modulos_for_equal_client_and_stored_paths) => derivation_closure_check_read_derivations_and_verified_hash_modulos_for_equal_client_and_stored_paths,
            Err(error) => {
                warn!(%project, error = %error, "The closure check failed for the derivation files that were reported.");
                self.mark_evaluation_failed_with_message(
                    job.evaluation_id,
                    &mapper.rewrite_stored_store_paths_in_text_to_outbound(&format!(
                        "The server rejected the derivation file that was reported: {error:#}"
                    )),
                )
                .await;
                return Ok(());
            }
        };
        for batch_entry in &mut derivations {
            batch_entry.closure_checked_hash_modulo = StorePath::parse_stored(
                batch_entry
                    .store_path_verified_derivation_record
                    .derivation_file_store_path(),
            )
            .ok()
            .and_then(|path| {
                derivation_closure_check_read_derivations_and_verified_hash_modulos_for_equal_client_and_stored_paths
                    .verified_hash_modulo_by_stored_derivation_path_for_equal_client_and_stored_paths
                    .get(&path.full())
                    .cloned()
            });
        }

        let report = match self
            .graph
            .record(RecordBatch {
                evaluation: job.evaluation_id,
                task: job.task_id,
                derivations,
                warnings,
                errors,
                continuation: job.continuation,
            })
            .await
        {
            Ok(report) => report,
            Err(graph_writer_error) => {
                self.record_report_batch_save_failure_and_mark_evaluation_failed(
                    job_id,
                    job.evaluation_id,
                    &graph_writer_error,
                )
                .await;
                return Err(GraphWriterDidNotSaveReportBatchError {
                    evaluation_id: job.evaluation_id,
                    graph_writer_error,
                }
                .into());
            }
        };
        if report.exec_entry_points > 0 {
            self.refresh_pre_warm().await;
        }

        Ok(())
    }

    async fn load_trusted_evaluation_batch_derivation(
        &self,
        project: ProjectId,
        reported: DiscoveredDerivation,
    ) -> Result<EvaluationBatchDerivation, String> {
        let drv_path = reported.drv_path;
        let stored_path = StorePath::parse_stored(&drv_path).map_err(|_| drv_path.clone())?;
        let base = stored_path.base();
        let record = match load_verified_trusted_derivation_record(
            &self.db.worker_db,
            &self.db.storage.nar_storage,
            Arc::clone(&self.db.store_path_mapping),
            project,
            &base,
        )
        .await
        {
            Ok(Some(record)) => record,
            Ok(None) => return Err(stored_path.full()),
            Err(error) => {
                warn!(%project, drv_path = %base, error = %error, "Failed to read the derivation file from storage or validate it");
                return Err(stored_path.full());
            }
        };

        Ok(EvaluationBatchDerivation {
            store_path_verified_derivation_record: record,
            attr: reported.attr,
            ifd: reported.ifd,
            licenses: reported.licenses,
            exec: reported.exec,
            closure_checked_hash_modulo: None,
        })
    }

    async fn record_report_batch_save_failure_and_mark_evaluation_failed(
        &self,
        job_id: &str,
        evaluation_id: EvaluationId,
        graph_writer_error: &anyhow::Error,
    ) {
        let report_batch_not_saved_failure_message =
            derivation_save_failure_user_message(graph_writer_error);
        let job_id = job_id.to_owned();
        let store_lost_report_batch_on_active_job_result = self
            .call(|reply| SchedulerMsg::MarkEvaluationJobReportBatchLost {
                job_id,
                message: report_batch_not_saved_failure_message.clone(),
                reply,
            })
            .await;
        if let Err(error) = store_lost_report_batch_on_active_job_result {
            warn!(%evaluation_id, error = %error, "failed to record on the evaluation job that the scheduler did not save a report batch");
        }
        self.mark_evaluation_failed_with_message(
            evaluation_id,
            &report_batch_not_saved_failure_message,
        )
        .await;
    }

    async fn mark_evaluation_failed_with_message(
        &self,
        evaluation_id: EvaluationId,
        message: &str,
    ) {
        match by_id::<EEvaluation, _>(&self.db.worker_db, evaluation_id).await {
            Ok(Some(eval)) => {
                if let Err(e) = update_evaluation_status_with_error(
                    &self.db,
                    eval,
                    EvaluationStatus::Failed,
                    &EvaluationFailure::GraphInsert(message),
                )
                .await
                {
                    warn!(error = %e, %evaluation_id, "failed to fail the evaluation");
                    return;
                }
                self.cancel_evaluation_jobs(evaluation_id, &[]).await;
            }
            Ok(None) => warn!(%evaluation_id, "evaluation not found for status update"),
            Err(e) => {
                warn!(error = %e, %evaluation_id, "failed to fetch evaluation for status update")
            }
        }
    }

    pub async fn record_eval_message(
        &self,
        job_id: &str,
        level: gradient_wire::types::EvalMessageLevel,
        source: String,
        message: String,
    ) -> Result<()> {
        let Some(evaluation_id) = self.active_job(job_id).await.map(|j| j.evaluation_id()) else {
            debug!(%job_id, "EvalMessage dropped: no active job");
            return Ok(());
        };

        let entity_level = match level {
            gradient_wire::types::EvalMessageLevel::Error => {
                gradient_entity::evaluation_message::MessageLevel::Error
            }
            gradient_wire::types::EvalMessageLevel::Warning => {
                gradient_entity::evaluation_message::MessageLevel::Warning
            }
            gradient_wire::types::EvalMessageLevel::Notice => {
                gradient_entity::evaluation_message::MessageLevel::Notice
            }
        };

        gradient_db::status::insert_worker_message(
            &self.db.worker_db,
            evaluation_id,
            entity_level,
            message,
            source,
        )
        .await
        .map_err(Into::into)
    }
}

struct ReportedInputDerivationFileAndHashModuloSource<'a> {
    db: &'a gradient_db::DbContext,
    reported: HashMap<String, StorePathVerifiedDerivationRecord>,
}

impl DrvSource for ReportedInputDerivationFileAndHashModuloSource<'_> {
    type Drv = StorePathVerifiedDerivationRecord;

    async fn read(&self, drv_path: &str) -> anyhow::Result<Option<Self::Drv>> {
        Ok(self.reported.get(drv_path).cloned())
    }

    async fn read_stored_derivation_hash_modulo_text_for_drv_paths(
        &self,
        drv_paths: &[String],
    ) -> anyhow::Result<HashMap<String, String>> {
        load_stored_derivation_hash_modulo_text_by_store_path_for_chunk(
            &self.db.worker_db,
            drv_paths,
        )
        .await
    }

    fn error_for_input_derivation_without_readable_drv_or_stored_verified_hash(
        &self,
        input: &str,
        dependent: &str,
    ) -> anyhow::Error {
        anyhow::anyhow!(
            "{input} is an input derivation of {dependent}. The evaluation worker has not reported the `.drv` file for {input}. The database does not hold a verified hash modulo for {input}. Two causes are possible.\n\nEvaluation workers older than the server report dependent derivations first. Upgrade the evaluation worker together with the server.\n\nThe cleanup of old unreferenced derivation rows deleted the row for {input} during this evaluation. Run the evaluation again."
        )
    }
}
