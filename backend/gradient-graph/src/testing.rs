/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use clap::Parser as _;
use gradient_db::evaluations::counters::recount_evaluations;
use gradient_db::graph::can_start::{lock_seed_shared_builds, seed_blocking_deps};
use gradient_db::graph::consistency::{ConsistencyReport, graph_consistency_report};
use gradient_db::{DbContext, IN_CHUNK_SIZE, WebDb, WorkerDb};
use gradient_entity::build::BuildStatus;
use gradient_entity::ids::{DerivationId, EvaluationId};
use gradient_entity::{derivation_build, evaluation};
use gradient_fixtures::database::TestDatabase;
use gradient_storage::{FileLogStorage, NarStore, StorageCtx};
use gradient_types::{Cli, RuntimeConfig};
use gradient_util::shutdown::Shutdown;
use sea_orm::{DatabaseConnection, TransactionTrait};

pub async fn ctx_with_crypt_file(db: DatabaseConnection, crypt_file: &str) -> DbContext {
    let dir = std::env::temp_dir().join(format!("gradient-graph-{}", uuid::Uuid::now_v7()));
    let cli = Cli::try_parse_from([
        "gradient-server",
        "--secrets-crypt-file",
        crypt_file,
        "--secrets-jwt-file",
        "test-jwt",
        "--serve-url",
        "http://127.0.0.1:3000",
        "--base-dir",
        dir.to_str().expect("a UTF-8 temp dir"),
    ])
    .expect("test cli");
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("test config"));
    DbContext {
        worker_db: WorkerDb::new(db.clone()),
        web_db: WebDb::new(db),
        config,
        storage: StorageCtx {
            nar_storage: NarStore::local(&dir).expect("the NAR store opens"),
            log_storage: Arc::new(
                FileLogStorage::new(&dir)
                    .await
                    .expect("the log storage opens"),
            ),
        },
        shutdown: Shutdown::new(),
        events: gradient_types::EventBus::new(16),
        wake: Default::default(),
        memory: Default::default(),
    }
}

pub fn secret_file() -> (tempfile::NamedTempFile, String) {
    use std::io::Write as _;

    let mut file = tempfile::NamedTempFile::new().expect("temp secret");
    file.write_all(b"test-secret-key-32-bytes-padding!")
        .expect("write secret");
    let path = file.path().to_string_lossy().to_string();
    (file, path)
}

pub fn signing_key(secret_path: &str) -> String {
    gradient_sources::generate_signing_key(secret_path)
        .expect("signing key")
        .0
}

pub async fn settle_seed(ctx: &DbContext, db: &TestDatabase, declared: Vec<(String, BuildStatus)>) {
    graph_consistency_report(ctx)
        .await
        .expect("the seed settles");

    write_blocking_deps_of_every_build(ctx, db).await;
    assert_declared_statuses(db, declared).await;
    write_counters_of_finished_evaluations(ctx, db).await;
}

pub async fn drift(ctx: &DbContext) -> ConsistencyReport {
    graph_consistency_report(ctx)
        .await
        .expect("the consistency check runs")
}

pub async fn zero_drift(ctx: &DbContext, after: &str) {
    let report = drift(ctx).await;
    assert_eq!(report.total(), 0, "drift after {after}: {report:?}");
}

async fn assert_declared_statuses(db: &TestDatabase, declared: Vec<(String, BuildStatus)>) {
    for (derivation, status) in declared {
        let settled = db.shared_build(&derivation).await.status;
        assert_eq!(
            settled, status,
            "seed of {derivation}: declared {status:?}, production settles it to {settled:?}"
        );
    }
}

async fn write_blocking_deps_of_every_build(ctx: &DbContext, db: &TestDatabase) {
    let derivations: Vec<DerivationId> = db
        .rows::<derivation_build::Entity>()
        .await
        .iter()
        .map(|build| build.derivation)
        .collect();

    for chunk in derivations.chunks(IN_CHUNK_SIZE) {
        let txn = ctx.worker_db.begin().await.expect("a transaction opens");
        let lock = lock_seed_shared_builds(&txn, chunk)
            .await
            .expect("the seeded builds lock");

        seed_blocking_deps(&lock)
            .await
            .expect("the blocking dependencies are written");

        txn.commit().await.expect("the transaction commits");
    }
}

async fn write_counters_of_finished_evaluations(ctx: &DbContext, db: &TestDatabase) {
    let finished: Vec<EvaluationId> = db
        .rows::<evaluation::Entity>()
        .await
        .iter()
        .filter(|evaluation| evaluation.status.is_terminal())
        .map(|evaluation| evaluation.id)
        .collect();

    recount_evaluations(&ctx.worker_db, &finished)
        .await
        .expect("the counters of the finished evaluations are written");
}
