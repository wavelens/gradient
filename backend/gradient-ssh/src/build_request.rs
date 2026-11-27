/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::build_wait::{BuildOutcome, wait};
use crate::session::Session;
use gradient_db::Hold;
use gradient_db::build_request_task::ensure_build_request_task;
use gradient_db::lookup::by_id;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_graph::derivation_closure_input_addressed_output_verification::{
    DerivationClosureCheckReadFilesAndHashModulosForClientPathEqualToStoredPath,
    DerivationFileInBuildRequestClosure, DrvSource,
    load_stored_derivation_hash_modulo_text_by_store_path_for_chunk, normalize_store_paths,
    read_transitive_input_derivations_checking_input_addressed_output_paths,
};
use gradient_graph::read_derivation_from_trusted_nar::{
    StorePathVerifiedDerivationRecord, load_verified_trusted_derivation_record,
};
use gradient_graph::{EvaluationBatchDerivation, RecordBatch, Transition};
use gradient_types::permissions::Permission;
use gradient_types::*;
use gradient_wire::types::DerivationOutput;
use harmonia_protocol::log::LogMessage;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, Select,
    TransactionTrait,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const BUILD_LOG_MESSAGE_BUFFER_SIZE_PER_SSH_CLIENT: usize = 4096;
const RECORD_BATCH_SIZE: usize = 50;

static EVALUATION_LOOKUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const JOINABLE: [EvaluationStatus; 3] = [
    EvaluationStatus::EvaluatingDerivation,
    EvaluationStatus::Building,
    EvaluationStatus::Waiting,
];

struct Target {
    task: TaskId,
    evaluation: EvaluationId,
    hold: Hold,
}

enum Added {
    Yes,
    EvaluationEnded,
}

pub(crate) struct VerifiedProjectDrvSource<'a> {
    pub(crate) session: &'a Session,
}

impl DrvSource for VerifiedProjectDrvSource<'_> {
    type Drv = StorePathVerifiedDerivationRecord;

    async fn read(&self, drv_path: &str) -> anyhow::Result<Option<Self::Drv>> {
        let db = &self.session.handles.db;
        load_verified_trusted_derivation_record(
            &db.web_db,
            &db.storage.nar_storage,
            Arc::clone(&db.store_path_mapping),
            self.session.project.id,
            drv_path,
        )
        .await
    }

    async fn read_stored_derivation_hash_modulo_text_for_drv_paths(
        &self,
        drv_paths: &[String],
    ) -> anyhow::Result<HashMap<String, String>> {
        load_stored_derivation_hash_modulo_text_by_store_path_for_chunk(
            &self.session.handles.db.web_db,
            drv_paths,
        )
        .await
    }

    fn error_for_input_derivation_without_readable_drv_or_stored_verified_hash(
        &self,
        input: &str,
        dependent: &str,
    ) -> anyhow::Error {
        input_drv_not_in_project_cache_error(input, dependent)
    }
}

fn input_drv_not_in_project_cache_error(input: &str, dependent: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{input} is an input derivation of {dependent}. {input} isn't in the project cache and the database row for {input} doesn't store a verified hash modulo. Copy {input} into the project cache first."
    )
}

pub fn drv_name(drv_path: &str) -> String {
    let name = StorePath::parse_stored(drv_path)
        .map_or_else(|_| drv_path.to_owned(), |path| path.name().to_owned());
    match name.strip_suffix(".drv") {
        Some(name) => name.to_owned(),
        None => name,
    }
}

pub async fn closure<S: DrvSource>(
    source: &S,
    roots: &[String],
) -> anyhow::Result<
    DerivationClosureCheckReadFilesAndHashModulosForClientPathEqualToStoredPath<S::Drv>,
> {
    let found =
        read_transitive_input_derivations_checking_input_addressed_output_paths(source, roots)
            .await?;
    for (path, read) in &found.read_stored_derivation_path_and_derivation_file_record_pairs {
        anyhow::ensure!(
            read.stored_derivation()
                .outputs
                .iter()
                .all(|o| !o.path.is_empty()),
            "{path} is content-addressed, not supported over SSH"
        );
    }
    Ok(found)
}

#[derive(Debug)]
pub struct Started {
    pub evaluation: EvaluationId,
    pub closure: Vec<String>,
    pub requested: Vec<(String, Vec<DerivationOutput>)>,
}

pub async fn start(session: &Session, drv_paths: &[String]) -> anyhow::Result<Started> {
    anyhow::ensure!(
        session.may(Permission::TriggerEvaluation),
        "building in {} needs TriggerEvaluation",
        session.project.name
    );

    let drv_paths = normalize_store_paths(drv_paths)?;
    let closure = closure(&VerifiedProjectDrvSource { session }, &drv_paths).await?;
    let evaluation = add_to_user_evaluation(session, &closure, &drv_paths).await?;

    let requested: HashSet<&str> = drv_paths.iter().map(String::as_str).collect();
    let paths = closure
        .read_stored_derivation_path_and_derivation_file_record_pairs
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    let requested_drvs = closure
        .read_stored_derivation_path_and_derivation_file_record_pairs
        .into_iter()
        .filter(|(path, _)| requested.contains(path.as_str()))
        .map(|(path, record)| (path, record.to_discovered_derivation().outputs))
        .collect();
    Ok(Started {
        evaluation,
        closure: paths,
        requested: requested_drvs,
    })
}

async fn add_to_user_evaluation(
    session: &Session,
    closure: &DerivationClosureCheckReadFilesAndHashModulosForClientPathEqualToStoredPath<
        StorePathVerifiedDerivationRecord,
    >,
    drv_paths: &[String],
) -> anyhow::Result<EvaluationId> {
    let target = evaluation_for(session, drv_paths).await?;
    let evaluation = target.evaluation;
    if let Added::Yes = add(session, target, closure, drv_paths).await? {
        return Ok(evaluation);
    }

    let target = evaluation_for(session, drv_paths).await?;
    let evaluation = target.evaluation;
    match add(session, target, closure, drv_paths).await? {
        Added::Yes => Ok(evaluation),
        Added::EvaluationEnded => {
            anyhow::bail!("evaluation {evaluation} ended before the builds were added")
        }
    }
}

async fn evaluation_for(session: &Session, drv_paths: &[String]) -> anyhow::Result<Target> {
    let _lookup = EVALUATION_LOOKUP.lock().await;
    let handles = &session.handles;
    let task = ensure_build_request_task(
        &handles.db.web_db,
        session.project.id,
        session.user.id,
        handles.db.config.eval.default_keep_evaluations(),
    )
    .await?;

    let running = running_evaluation(
        task.id,
        session.user.id,
        session.allows_project_credentials_and_privileged_actions,
    )
    .one(&handles.db.web_db)
    .await?;
    let evaluation = match running {
        Some(running) => running.id,
        None => create_evaluation(session, task.id, drv_paths).await?,
    };

    Ok(Target {
        task: task.id,
        evaluation,
        hold: handles.db.memory.held_evaluations.hold(evaluation).await,
    })
}

fn running_evaluation(
    task: TaskId,
    user: UserId,
    allows_project_credentials_and_privileged_actions: bool,
) -> Select<EEvaluation> {
    EEvaluation::find()
        .filter(CEvaluation::Task.eq(task))
        .filter(CEvaluation::Kind.eq(EvaluationKind::Ssh))
        .filter(CEvaluation::StartedBy.eq(user))
        .filter(
            CEvaluation::AllowsProjectCredentialsAndPrivilegedActions
                .eq(allows_project_credentials_and_privileged_actions),
        )
        .filter(CEvaluation::Status.is_in(JOINABLE))
        .order_by_desc(CEvaluation::CreatedAt)
}

async fn create_evaluation(
    session: &Session,
    task: TaskId,
    drv_paths: &[String],
) -> anyhow::Result<EvaluationId> {
    let handles = &session.handles;

    let names: Vec<String> = drv_paths.iter().map(|p| drv_name(p)).collect();
    let tx = handles.db.web_db.inner().begin().await?;
    let commit = MCommit {
        id: CommitId::now_v7(),
        message: format!("SSH build of {}", names.join(" ")),
        hash: vec![0; 20],
        author: Some(session.user.id),
        author_name: session.user.name.clone(),
        ..Default::default()
    }
    .into_active_model()
    .insert(&tx)
    .await?;

    let created = now();
    let evaluation = MEvaluation {
        id: EvaluationId::now_v7(),
        task: Some(task),
        repository: "ssh".into(),
        commit: commit.id,
        wildcard: names.join(" "),
        status: EvaluationStatus::EvaluatingDerivation,
        kind: EvaluationKind::Ssh,
        started_by: Some(session.user.id),
        concurrent: true,
        branch: gradient_types::branch_key::NO_BRANCH.to_owned(),
        created_at: created,
        updated_at: created,
        eval_drv_started_at: Some(created),
        allows_project_credentials_and_privileged_actions: session
            .allows_project_credentials_and_privileged_actions,
        ..Default::default()
    }
    .into_active_model()
    .insert(&tx)
    .await?;
    tx.commit().await?;

    Ok(evaluation.id)
}

async fn add(
    session: &Session,
    target: Target,
    closure: &DerivationClosureCheckReadFilesAndHashModulosForClientPathEqualToStoredPath<
        StorePathVerifiedDerivationRecord,
    >,
    drv_paths: &[String],
) -> anyhow::Result<Added> {
    let Target {
        task,
        evaluation,
        hold,
        ..
    } = target;
    let mut added = add_batches(session, task, evaluation, closure, drv_paths).await;
    if matches!(added, Ok(Added::Yes)) && ended(session, evaluation).await? {
        added = Ok(Added::EvaluationEnded);
    }
    drop(hold);

    if !matches!(added, Ok(Added::EvaluationEnded)) {
        session
            .handles
            .graph
            .transition(Transition::EvalStreamCompleted { evaluation })
            .await?;
    }
    added
}

async fn ended(session: &Session, evaluation: EvaluationId) -> anyhow::Result<bool> {
    Ok(
        by_id::<EEvaluation, _>(&session.handles.db.web_db, evaluation)
            .await?
            .is_none_or(|e| EvaluationStatus::TERMINAL.contains(&e.status)),
    )
}

async fn add_batches(
    session: &Session,
    task: TaskId,
    evaluation: EvaluationId,
    closure: &DerivationClosureCheckReadFilesAndHashModulosForClientPathEqualToStoredPath<
        StorePathVerifiedDerivationRecord,
    >,
    drv_paths: &[String],
) -> anyhow::Result<Added> {
    let handles = &session.handles;
    let requested: HashSet<&str> = drv_paths.iter().map(String::as_str).collect();
    let derivations: Vec<EvaluationBatchDerivation> = closure
        .read_stored_derivation_path_and_derivation_file_record_pairs
        .iter()
        .map(|(path, record)| EvaluationBatchDerivation {
            attr: entry_point_name_for_requested_closure_entry(&requested, path),
            ifd: false,
            licenses: record.to_discovered_derivation().licenses,
            exec: None,
            store_path_verified_derivation_record: record.clone(),
            closure_checked_hash_modulo: closure
                .verified_hash_modulo_by_stored_derivation_path_for_equal_client_and_stored_paths
                .get(path)
                .cloned(),
        })
        .collect();
    for batch in derivations.chunks(RECORD_BATCH_SIZE) {
        let report = handles
            .graph
            .record(RecordBatch {
                evaluation,
                task: Some(task),
                derivations: batch.to_vec(),
                warnings: vec![],
                errors: vec![],
                continuation: None,
            })
            .await
            .map_err(|e| {
                tracing::warn!(%evaluation, error = format!("{e:#}"), "Failed to save the batch of derivations for the request to build over SSH.");
                anyhow::anyhow!(gradient_graph::writer::derivation_save_failure_user_message(&e))
            })?;
        if report.skipped {
            return Ok(Added::EvaluationEnded);
        }
    }

    Ok(Added::Yes)
}

// Entry points are unique per evaluation and name; many derivations share a name like `source`.
fn entry_point_name_for_requested_closure_entry(requested: &HashSet<&str>, path: &str) -> String {
    if !requested.contains(path) {
        return String::new();
    }
    let base = path.rsplit_once('/').map_or(path, |(_, base)| base);
    base.strip_suffix(".drv").unwrap_or(base).to_owned()
}

pub async fn run(
    session: &Session,
    drv_paths: &[String],
    log: impl Fn(LogMessage) + Send + Sync,
) -> anyhow::Result<BuildOutcome> {
    let started = start(session, drv_paths).await?;
    log(LogMessage::message(format!(
        "Gradient evaluation {}",
        started.evaluation
    )));
    wait(session, &started, log).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SshHandles;
    use gradient_derivation::{Derivation, DerivationOutput, discovered_derivation, parse_drv};
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_graph::derivation_closure_input_addressed_output_verification::check_input_addressed_output_paths;
    use gradient_test_support::state::TestState;
    use gradient_wire::types::DiscoveredDerivation;
    use harmonia_store_aterm::parse_derivation_aterm;
    use harmonia_store_path::{StoreDir, StorePath as ClientPath};

    use std::sync::Mutex;

    #[derive(Debug, Clone)]
    struct CacheRewrittenDerivationWithOriginalDrv {
        stored_derivation: Derivation,
        original_drv_store_path: ClientPath,
        derivation_with_original_paths: harmonia_store_derivation::derivation::Derivation,
    }

    impl DerivationFileInBuildRequestClosure for CacheRewrittenDerivationWithOriginalDrv {
        fn stored_derivation(&self) -> DiscoveredDerivation {
            discovered_derivation(
                None,
                self.original_drv_store_path.to_string(),
                &self.stored_derivation,
            )
        }

        fn original_drv_store_path(&self) -> anyhow::Result<ClientPath> {
            Ok(self.original_drv_store_path.clone())
        }

        fn derivation_with_original_paths(
            &self,
        ) -> anyhow::Result<harmonia_store_derivation::derivation::Derivation> {
            Ok(self.derivation_with_original_paths.clone())
        }
    }

    #[derive(Default)]
    struct Fake {
        drvs: HashMap<String, CacheRewrittenDerivationWithOriginalDrv>,
        reads: Mutex<Vec<String>>,
        hash_modulo_text_by_drv_store_path: HashMap<String, String>,
        drv_path_count_per_hash_per_lookup_call: Mutex<Vec<usize>>,
    }

    impl DrvSource for Fake {
        type Drv = CacheRewrittenDerivationWithOriginalDrv;

        async fn read(&self, drv_path: &str) -> anyhow::Result<Option<Self::Drv>> {
            self.reads.lock().expect("reads").push(drv_path.to_string());
            Ok(self.drvs.get(drv_path).cloned())
        }

        async fn read_stored_derivation_hash_modulo_text_for_drv_paths(
            &self,
            drv_paths: &[String],
        ) -> anyhow::Result<HashMap<String, String>> {
            self.drv_path_count_per_hash_per_lookup_call
                .lock()
                .expect("lookups")
                .push(drv_paths.len());
            Ok(drv_paths
                .iter()
                .filter_map(|path| {
                    let text = self.hash_modulo_text_by_drv_store_path.get(path)?;
                    Some((path.clone(), text.clone()))
                })
                .collect())
        }

        fn error_for_input_derivation_without_readable_drv_or_stored_verified_hash(
            &self,
            input: &str,
            dependent: &str,
        ) -> anyhow::Error {
            input_drv_not_in_project_cache_error(input, dependent)
        }
    }

    fn client_path(path: &str) -> ClientPath {
        ClientPath::from_base_path(path.trim().trim_start_matches("/nix/store/")).expect("path")
    }

    fn path(name: &str) -> String {
        format!("/nix/store/{}-{name}.drv", "0".repeat(32))
    }

    fn drv(inputs: &[&str]) -> Derivation {
        Derivation {
            outputs: vec![DerivationOutput {
                name: "out".into(),
                path: format!("/nix/store/{}-out", "1".repeat(32)),
                hash_algo: String::new(),
                hash: String::new(),
            }],
            input_derivations: inputs
                .iter()
                .map(|i| gradient_derivation::InputDrv::new(path(i), vec!["out".into()]))
                .collect(),
            input_sources: vec![],
            system: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec![],
            environment: HashMap::new(),
        }
    }

    fn fake(drvs: &[(&str, Derivation)]) -> Fake {
        let read = |name: &str, drv: &Derivation| CacheRewrittenDerivationWithOriginalDrv {
            stored_derivation: drv.clone(),
            original_drv_store_path: client_path(&path(name)),
            derivation_with_original_paths: harmonia_store_derivation::derivation::Derivation::new(
                name.parse().expect("name"),
                "x86_64-linux".into(),
                "/bin/sh".into(),
            ),
        };
        Fake {
            drvs: drvs.iter().map(|(n, d)| (path(n), read(n, d))).collect(),
            reads: Mutex::new(Vec::new()),
            ..Fake::default()
        }
    }

    const DEP_ATERM: &[u8] = include_bytes!("../testdata/input-addressed-dep.drv");
    const DEP_PATH: &str = include_str!("../testdata/input-addressed-dep.drv.path");
    const TOP_ATERM: &[u8] = include_bytes!("../testdata/input-addressed-top.drv");
    const TOP_PATH: &str = include_str!("../testdata/input-addressed-top.drv.path");

    fn fake_derivation_source_from_aterm_files_with_original_store_paths(
        drvs: &[(&str, &[u8])],
    ) -> Fake {
        let read = |drv_path: &str, aterm: &[u8]| {
            let original_drv_store_path = client_path(drv_path);
            let name = original_drv_store_path.name().to_string();
            CacheRewrittenDerivationWithOriginalDrv {
                stored_derivation: parse_drv(aterm).expect("drv"),
                derivation_with_original_paths: parse_derivation_aterm(
                    &StoreDir::default(),
                    aterm,
                    name.strip_suffix(".drv")
                        .expect("drv")
                        .parse()
                        .expect("name"),
                )
                .expect("aterm"),
                original_drv_store_path,
            }
        };
        Fake {
            drvs: drvs
                .iter()
                .map(|(p, aterm)| (p.trim().to_owned(), read(p, aterm)))
                .collect(),
            reads: Mutex::new(Vec::new()),
            ..Fake::default()
        }
    }

    fn drv_aterm_text_with_out_output_path_changed_by_one_hash_character(aterm: &[u8]) -> Vec<u8> {
        let text = std::str::from_utf8(aterm).expect("utf8");
        let marker = "(\"out\",\"/nix/store/";
        let start = text.find(marker).expect("out output") + marker.len() - "/nix/store/".len();
        let end = start + text[start..].find('"').expect("path end");
        let stated = &text[start..end];
        let hash_start = "/nix/store/".len();
        let replacement = if &stated[hash_start..=hash_start] == "a" {
            "b"
        } else {
            "a"
        };
        let forged = format!(
            "{}{replacement}{}",
            &stated[..hash_start],
            &stated[hash_start + 1..]
        );
        text.replace(stated, &forged).into_bytes()
    }

    #[tokio::test]
    async fn unchanged_real_input_addressed_derivation_and_its_input_derivation_pass_output_path_check()
     {
        let source = fake_derivation_source_from_aterm_files_with_original_store_paths(&[
            (TOP_PATH, TOP_ATERM),
            (DEP_PATH, DEP_ATERM),
        ]);

        let closure = closure(&source, &[TOP_PATH.trim().to_owned()])
            .await
            .expect("closure");

        assert_eq!(
            closure
                .read_stored_derivation_path_and_derivation_file_record_pairs
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn derivation_output_path_changed_to_other_valid_store_path_is_refused_and_refusal_names_output()
     {
        let forged = drv_aterm_text_with_out_output_path_changed_by_one_hash_character(TOP_ATERM);
        let source = fake_derivation_source_from_aterm_files_with_original_store_paths(&[
            (TOP_PATH, &forged),
            (DEP_PATH, DEP_ATERM),
        ]);

        let e = closure(&source, &[TOP_PATH.trim().to_owned()])
            .await
            .expect_err("forged output path");

        assert!(e.to_string().contains("shows output out at"), "{e}");
    }

    #[tokio::test]
    async fn requesting_top_drv_is_refused_when_input_derivation_dep_drv_states_forged_output_path()
    {
        let forged = drv_aterm_text_with_out_output_path_changed_by_one_hash_character(DEP_ATERM);
        let source = fake_derivation_source_from_aterm_files_with_original_store_paths(&[
            (TOP_PATH, TOP_ATERM),
            (DEP_PATH, &forged),
        ]);

        let e = closure(&source, &[TOP_PATH.trim().to_owned()])
            .await
            .expect_err("forged input output path");

        assert!(
            e.to_string()
                .contains(DEP_PATH.trim().trim_start_matches("/nix/store/")),
            "{e}"
        );
        assert!(e.to_string().contains("shows output out at"), "{e}");
    }

    fn build_test_derivation_source_with_top_derivation_and_dependency_hash_modulo_text(
        dependency_hash_modulo_text: Option<&str>,
    ) -> Fake {
        let mut source = fake_derivation_source_from_aterm_files_with_original_store_paths(&[(
            TOP_PATH, TOP_ATERM,
        )]);
        source.hash_modulo_text_by_drv_store_path = dependency_hash_modulo_text
            .map(|text| (DEP_PATH.trim().to_owned(), text.to_owned()))
            .into_iter()
            .collect();
        source
    }

    #[tokio::test]
    async fn closure_check_of_top_derivation_with_unreadable_input_drv_uses_stored_hash_modulo_and_reads_only_top_file_with_one_lookup()
     {
        let source =
            build_test_derivation_source_with_top_derivation_and_dependency_hash_modulo_text(Some(
                "drv:sha256:2f147701429b5cb2d91db5743a8edbab066eedeb5f5a7afe7c5be32def86670c",
            ));

        let found = closure(&source, &[TOP_PATH.trim().to_owned()])
            .await
            .expect("The stored hash modulo of the input can substitute for that unreadable file.");

        assert_eq!(
            found
                .read_stored_derivation_path_and_derivation_file_record_pairs
                .len(),
            1
        );
        assert_eq!(*source.reads.lock().expect("reads"), [TOP_PATH.trim()]);
        assert_eq!(
            *source
                .drv_path_count_per_hash_per_lookup_call
                .lock()
                .expect("lookups"),
            [1]
        );
    }

    #[tokio::test]
    async fn test_wrong_stored_hash_modulo_for_unreported_input_fails_top_derivation_output_path_check()
     {
        let source =
            build_test_derivation_source_with_top_derivation_and_dependency_hash_modulo_text(Some(
                "drv:sha256:93fde50a3163cc333d4ed43c059ac5b5a580b1dd33a6d88a6ab2d4988b54ddba",
            ));

        let e = closure(&source, &[TOP_PATH.trim().to_owned()])
            .await
            .expect_err(
                "An incorrect remainder of the stored hash can produce a different output path.",
            );

        assert!(e.to_string().contains("shows output out at"), "{e}");
    }

    #[tokio::test]
    async fn test_ssh_input_without_verified_hash_or_readable_drv_file_fails_closure_check_with_copy_input_to_project_cache_message()
     {
        let source =
            build_test_derivation_source_with_top_derivation_and_dependency_hash_modulo_text(None);

        let e = closure(&source, &[TOP_PATH.trim().to_owned()])
            .await
            .expect_err("Inputs with no stored value and no file fail.");

        assert!(
            e.to_string().contains(&format!(
                "Copy {} into the project cache first.",
                DEP_PATH.trim()
            )),
            "{e}"
        );
    }

    #[tokio::test]
    async fn lookup_five_thousand_unreported_inputs_of_one_derivation_takes_five_calls_of_at_most_one_thousand_paths_and_reads_only_root_file()
     {
        let inputs: Vec<String> = (0..5000).map(|index| format!("input{index}")).collect();
        let input_names: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let mut source = fake(&[("root", drv(&input_names))]);
        source.hash_modulo_text_by_drv_store_path = inputs
            .iter()
            .map(|name| (path(name), "deferred".to_owned()))
            .collect();

        closure(&source, &[path("root")])
            .await
            .expect("All inputs have a stored value.");

        assert_eq!(*source.reads.lock().expect("reads"), [path("root")]);
        let lookups = source
            .drv_path_count_per_hash_per_lookup_call
            .lock()
            .expect("lookups")
            .clone();
        assert_eq!(lookups, [1000; 5]);
    }

    #[test]
    fn output_path_check_refuses_derivation_with_missing_input_derivation_in_build_request_and_reports_output_path_cannot_be_checked()
     {
        let source = fake_derivation_source_from_aterm_files_with_original_store_paths(&[(
            TOP_PATH, TOP_ATERM,
        )]);
        let top = source.drvs[TOP_PATH.trim()].clone();
        let derivations = HashMap::from([(
            top.original_drv_store_path,
            top.derivation_with_original_paths,
        )]);

        let e = check_input_addressed_output_paths(&derivations, &HashMap::new())
            .expect_err("missing input");

        assert!(e.to_string().contains("Can't check the output path"), "{e}");
    }

    #[test]
    fn floating_content_addressed_output_with_store_path_is_rejected() {
        let text = std::str::from_utf8(DEP_ATERM).expect("utf8");
        let output_start = text.find("(\"out\",").expect("out output");
        let fields_end = output_start + text[output_start..].find(",\"\",\"\")").expect("fields");
        let floating = format!(
            "{}{}{}",
            &text[..fields_end],
            ",\"r:sha256\",\"\")",
            &text[fields_end + ",\"\",\"\")".len()..]
        );
        let parsed = parse_derivation_aterm(
            &StoreDir::default(),
            floating.as_bytes(),
            "dep".parse().expect("name"),
        );

        let e = parsed.expect_err("floating output with a stated path");

        assert!(format!("{e:?}").contains("InvalidCombination"), "{e:?}");
    }

    #[tokio::test]
    async fn closure_visits_each_drv_once() {
        let source = fake(&[("a", drv(&["b", "c"])), ("b", drv(&["c"])), ("c", drv(&[]))]);

        let closure = closure(&source, &[path("a")]).await.expect("closure");

        let mut found: Vec<String> = closure
            .read_stored_derivation_path_and_derivation_file_record_pairs
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        found.sort();
        assert_eq!(found, [path("a"), path("b"), path("c")]);
        assert_eq!(source.reads.lock().expect("reads").len(), 3);
    }

    #[tokio::test]
    async fn a_drv_missing_from_the_cache_names_the_path() {
        let source = fake(&[("a", drv(&["b"]))]);

        let e = closure(&source, &[path("a")]).await.expect_err("missing");
        assert!(e.to_string().contains(&path("b")), "{e}");
        assert!(e.to_string().contains("Copy"), "{e}");
    }

    #[tokio::test]
    async fn a_content_addressed_derivation_is_refused() {
        let mut floating = drv(&[]);
        floating.outputs[0].path = String::new();
        let source = fake(&[("a", floating)]);

        let e = closure(&source, &[path("a")]).await.expect_err("ca");
        assert!(e.to_string().contains("content-addressed"), "{e}");
    }

    struct TestDatabaseWithOneProjectBuiltContentVariantAndLocalNarStorage {
        db: gradient_fixtures::database::TestDatabase,
        nar_storage_dir: std::path::PathBuf,
        trusting: ProjectId,
        other: ProjectId,
    }

    impl TestDatabaseWithOneProjectBuiltContentVariantAndLocalNarStorage {
        async fn new(stored_text: &[u8]) -> Self {
            use gradient_fixtures::shape::shape;
            use gradient_fixtures::store_path_content_variants::{BUILD_A, BUILD_B};
            use gradient_test_support::cache_fixture::zstd_compressed_nar_of_bytes;

            let base = DEP_PATH.trim().trim_start_matches("/nix/store/");
            let (hash, package) = base.split_once('-').expect("base path");
            let (trusting, other) = (EvaluationId::now_v7(), EvaluationId::now_v7());
            let db = shape()
                .evaluation(trusting)
                .derivation(BUILD_A)
                .job()
                .cached_path(hash, package)
                .evaluation(other)
                .derivation(BUILD_B)
                .job()
                .into_database()
                .await;
            let nar_storage_dir =
                std::env::temp_dir().join(format!("gradient-ssh-drv-{}", uuid::Uuid::now_v7()));
            gradient_storage::NarStore::local(&nar_storage_dir)
                .expect("local NAR storage")
                .put(
                    &format!("{hash}-abc"),
                    zstd_compressed_nar_of_bytes(stored_text).await,
                )
                .await
                .expect("store the variant");
            Self {
                trusting: db.project_of(trusting).await,
                other: db.project_of(other).await,
                db,
                nar_storage_dir,
            }
        }

        async fn open_ssh_session_for_project(&self, project: ProjectId) -> Session {
            let state = TestState::new()
                .web_db(self.db.connection())
                .nar_storage(
                    gradient_storage::NarStore::local(&self.nar_storage_dir)
                        .expect("local NAR storage"),
                )
                .build();
            Session::open(
                SshHandles::for_tests(&state),
                gradient_fixtures::rows::user(),
                MProject {
                    id: project,
                    ..gradient_fixtures::rows::project()
                },
                0,
                vec![],
                true,
            )
            .await
            .expect("session")
        }
    }

    #[tokio::test]
    async fn ssh_build_request_refuses_content_variant_trusted_by_other_project_but_passes_for_trusting_project()
     {
        let stored =
            TestDatabaseWithOneProjectBuiltContentVariantAndLocalNarStorage::new(DEP_ATERM).await;
        let roots = [DEP_PATH.trim().to_owned()];

        let trusting = stored.open_ssh_session_for_project(stored.trusting).await;
        let read = closure(&VerifiedProjectDrvSource { session: &trusting }, &roots)
            .await
            .expect("The requesting project built the only stored variant of the .drv, so the closure can read and check that variant.");
        let other = stored.open_ssh_session_for_project(stored.other).await;
        let e = closure(&VerifiedProjectDrvSource { session: &other }, &roots)
            .await
            .expect_err("A project cannot get derivations from a .drv if the project never built the only stored variant of that .drv.");

        assert_eq!(
            read.read_stored_derivation_path_and_derivation_file_record_pairs
                .len(),
            1
        );
        assert!(e.to_string().contains("copy it first"), "{e}");
        stored.db.finish().await;
    }

    #[tokio::test]
    async fn rejects_ssh_build_request_for_drv_whose_text_hash_does_not_match_store_path() {
        let forged = drv_aterm_text_with_out_output_path_changed_by_one_hash_character(DEP_ATERM);
        let stored =
            TestDatabaseWithOneProjectBuiltContentVariantAndLocalNarStorage::new(&forged).await;
        let session = stored.open_ssh_session_for_project(stored.trusting).await;

        let e = closure(
            &VerifiedProjectDrvSource { session: &session },
            &[DEP_PATH.trim().to_owned()],
        )
        .await
        .expect_err("The build can reject saved `.drv` texts with a hash not equal to their store path before the check of that output path.");

        assert!(
            format!("{e:#}").contains("does not match its store path"),
            "{e:#}"
        );
        stored.db.finish().await;
    }

    #[tokio::test]
    async fn building_without_trigger_evaluation_is_refused() {
        let db = empty_db();
        let session = session(db.clone()).await;

        let e = start(&session, &[path("a")]).await.expect_err("refused");
        assert!(e.to_string().contains("TriggerEvaluation"), "{e}");
        assert!(db.into_transaction_log().is_empty());
    }

    async fn session(db: sea_orm::DatabaseConnection) -> Session {
        session_with_project_credentials_and_privileged_actions_flag(db, true).await
    }

    async fn session_with_project_credentials_and_privileged_actions_flag(
        db: sea_orm::DatabaseConnection,
        allows_project_credentials_and_privileged_actions: bool,
    ) -> Session {
        Session::open(
            SshHandles::for_tests(&TestState::new().web_db(db).build()),
            gradient_fixtures::rows::user(),
            gradient_fixtures::rows::project(),
            0,
            vec![],
            allows_project_credentials_and_privileged_actions,
        )
        .await
        .unwrap()
    }

    fn build_request_task() -> MTask {
        MTask {
            id: TaskId::now_v7(),
            project: gradient_fixtures::rows::project().id,
            name: gradient_db::build_request_task::BUILD_REQUEST_TASK_NAME.into(),
            managed: true,
            ..Default::default()
        }
    }

    fn ssh_evaluation(task: &MTask) -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            task: Some(task.id),
            kind: EvaluationKind::Ssh,
            status: EvaluationStatus::Building,
            started_by: Some(gradient_fixtures::rows::user().id),
            ..Default::default()
        }
    }

    fn commit() -> MCommit {
        MCommit {
            id: CommitId::now_v7(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_second_request_joins_the_running_evaluation() {
        let task = build_request_task();
        let running = ssh_evaluation(&task);
        let db = mock_db()
            .append_query_results([vec![task.clone()]])
            .append_query_results([vec![running.clone()]])
            .into_connection();
        let session = session(db).await;

        let target = evaluation_for(&session, &[path("a")])
            .await
            .expect("target");

        assert_eq!(target.evaluation, running.id);
        assert!(
            session
                .handles
                .db
                .memory
                .held_evaluations
                .holds(running.id)
                .await
        );
        drop(target);
        assert!(
            !session
                .handles
                .db
                .memory
                .held_evaluations
                .holds(running.id)
                .await
        );
    }

    #[tokio::test]
    async fn a_request_without_a_running_evaluation_creates_an_evaluation() {
        let task = build_request_task();
        let created = ssh_evaluation(&task);
        let db = mock_db()
            .append_query_results([vec![task.clone()]])
            .append_query_results([Vec::<MEvaluation>::new()])
            .append_query_results([vec![commit()]])
            .append_query_results([vec![created.clone()]])
            .into_connection();
        let session = session(db).await;

        let target = evaluation_for(&session, &[path("a")])
            .await
            .expect("target");

        assert_eq!(target.evaluation, created.id);
        assert!(
            session
                .handles
                .db
                .memory
                .held_evaluations
                .holds(created.id)
                .await
        );
    }

    #[tokio::test]
    async fn grpc_build_request_writes_session_project_credentials_flag_into_evaluation_and_join_lookup()
     {
        for allows_project_credentials_and_privileged_actions in [true, false] {
            let task = build_request_task();
            let created = ssh_evaluation(&task);
            let db = mock_db()
                .append_query_results([vec![task.clone()]])
                .append_query_results([Vec::<MEvaluation>::new()])
                .append_query_results([vec![commit()]])
                .append_query_results([vec![created.clone()]])
                .into_connection();
            let session = session_with_project_credentials_and_privileged_actions_flag(
                db.clone(),
                allows_project_credentials_and_privileged_actions,
            )
            .await;

            drop(
                evaluation_for(&session, &[path("a")])
                    .await
                    .expect("target"),
            );

            let bound_flag =
                sea_orm::Value::Bool(Some(allows_project_credentials_and_privileged_actions));
            let log = db.into_transaction_log();
            let statements: Vec<_> = log.iter().flat_map(|t| t.statements()).collect();
            let insert = statements
                .iter()
                .find(|s| s.sql.starts_with("INSERT INTO \"evaluation\""))
                .expect("evaluation insert");
            let columns = &insert.sql[insert.sql.find('(').expect("columns") + 1
                ..insert.sql.find(')').expect("columns")];
            let flag = columns
                .split(", ")
                .position(|c| c == "\"allows_project_credentials_and_privileged_actions\"")
                .expect("flag column");
            assert_eq!(insert.values.as_ref().expect("values").0[flag], bound_flag);
            let lookup = statements
                .iter()
                .find(|s| s.sql.starts_with("SELECT") && s.sql.contains("FROM \"evaluation\""))
                .expect("running evaluation lookup");
            assert!(
                lookup
                    .values
                    .as_ref()
                    .expect("values")
                    .0
                    .contains(&bound_flag),
                "{:?}",
                lookup.values
            );
        }
    }

    #[tokio::test]
    async fn lookup_running_ssh_evaluation_for_join_returns_only_evaluation_matching_task_kind_starter_credentials_flag_and_joinable_status()
     {
        use sea_orm::EntityTrait;
        use sea_orm::sea_query::Expr;

        let (target, other_task) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let test_db = gradient_fixtures::shape::shape()
            .evaluation(target)
            .evaluation_status(EvaluationStatus::Building)
            .evaluation(other_task)
            .evaluation_status(EvaluationStatus::Building)
            .into_database()
            .await;
        let db = test_db.connection();
        let user = gradient_fixtures::rows::user();
        let stranger = gradient_db::rows::insert::<gradient_types::EUser, _>(
            &db,
            MUser {
                id: UserId::now_v7(),
                username: "stranger".into(),
                email: "stranger@example.com".into(),
                ..user.clone()
            },
        )
        .await
        .expect("stranger");
        EEvaluation::update_many()
            .col_expr(CEvaluation::StartedBy, Expr::value(user.id))
            .col_expr(CEvaluation::Kind, Expr::value(EvaluationKind::Ssh))
            .exec(&db)
            .await
            .expect("ssh evaluations");
        let matching = test_db
            .find::<gradient_entity::evaluation::Entity>(target)
            .await
            .expect("target");
        let finished_or_not_started = [
            EvaluationStatus::Queued,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::Fetching,
            EvaluationStatus::Completed,
            EvaluationStatus::Failed,
            EvaluationStatus::Aborted,
        ]
        .map(|status| MEvaluation {
            status,
            ..matching.clone()
        });
        let other_joinable = [
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Waiting,
        ]
        .map(|status| MEvaluation {
            id: EvaluationId::now_v7(),
            status,
            ..matching.clone()
        });
        let lookalikes = finished_or_not_started.into_iter().chain([
            MEvaluation {
                allows_project_credentials_and_privileged_actions: false,
                ..matching.clone()
            },
            MEvaluation {
                kind: EvaluationKind::Retry,
                ..matching.clone()
            },
            MEvaluation {
                started_by: Some(stranger.id),
                ..matching.clone()
            },
        ]);
        for lookalike in other_joinable.iter().cloned() {
            gradient_db::rows::insert::<EEvaluation, _>(
                &db,
                MEvaluation {
                    branch: format!("joinable-{}", lookalike.id),
                    ..lookalike
                },
            )
            .await
            .expect("joinable");
        }
        for (n, lookalike) in lookalikes.enumerate() {
            gradient_db::rows::insert::<EEvaluation, _>(
                &db,
                MEvaluation {
                    id: EvaluationId::now_v7(),
                    branch: format!("lookalike-{n}"),
                    ..lookalike
                },
            )
            .await
            .expect("lookalike");
        }

        let found = running_evaluation(test_db.task_of(target).await, user.id, true)
            .all(&db)
            .await
            .expect("lookup");

        let mut expected: Vec<_> = other_joinable
            .iter()
            .map(|e| e.id)
            .chain([target])
            .collect();
        let mut found: Vec<_> = found.into_iter().map(|e| e.id).collect();
        expected.sort();
        found.sort();
        assert_eq!(found, expected);
        test_db.finish().await;
    }

    #[test]
    fn requests_joining_one_evaluation_with_alike_named_derivations_keep_separate_entry_points() {
        let first = format!("/nix/store/{}-source.drv", "a".repeat(32));
        let second = format!("/nix/store/{}-source.drv", "b".repeat(32));

        let requested = HashSet::from([first.as_str(), second.as_str()]);
        let entry_point =
            |path: &String| entry_point_name_for_requested_closure_entry(&requested, path);

        assert_ne!(entry_point(&first), entry_point(&second));
        assert!(!entry_point(&first).is_empty());
    }

    #[test]
    fn drv_names_drop_the_hash_and_suffix() {
        assert_eq!(drv_name(&path("hello-2.12")), "hello-2.12");
    }
}
