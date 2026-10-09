/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_types::ids::DerivationId;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use tracing::warn;

#[derive(Debug, Default)]
pub struct MissingInputDiagnosis {
    pub cached_path_present: bool,
    pub fully_cached: bool,
    pub outputs_total: usize,
    pub outputs_cached: usize,
    pub producer_build_statuses: Vec<gradient_entity::build::BuildStatus>,
}

pub async fn diagnose_missing_input<C: ConnectionTrait>(
    db: &C,
    _evaluation_id: gradient_types::ids::EvaluationId,
    hash: &str,
) -> Result<MissingInputDiagnosis, sea_orm::DbErr> {
    use gradient_entity::cached_path::{Column as CCP, Entity as ECP};
    use gradient_entity::derivation_build::{Column as CDB, Entity as EDB};
    use gradient_entity::derivation_output::{Column as CDO, Entity as EDO};

    let cached_path = ECP::find().filter(CCP::Hash.eq(hash)).one(db).await?;
    let outputs = EDO::find().filter(CDO::Hash.eq(hash)).all(db).await?;
    let outputs_cached = outputs.iter().filter(|o| o.is_cached).count();
    let producer_drvs: Vec<DerivationId> = outputs.iter().map(|o| o.derivation).collect();

    let producer_build_statuses = if producer_drvs.is_empty() {
        Vec::new()
    } else {
        EDB::find()
            .filter(CDB::Derivation.is_in(producer_drvs))
            .all(db)
            .await?
            .into_iter()
            .map(|b| b.status)
            .collect()
    };

    Ok(MissingInputDiagnosis {
        cached_path_present: cached_path.is_some(),
        fully_cached: cached_path.map(|c| c.is_fully_cached()).unwrap_or(false),
        outputs_total: outputs.len(),
        outputs_cached,
        producer_build_statuses,
    })
}

fn demoted_output(
    o: gradient_entity::derivation_output::Model,
) -> gradient_entity::derivation_output::ActiveModel {
    use sea_orm::{ActiveValue::Set, IntoActiveModel};

    let mut active = o.into_active_model();
    active.is_cached = Set(false);
    active.cached_path = Set(None);
    active.external_url = Set(None);
    active.nar_hash = Set(None);
    active.file_hash = Set(None);
    active.file_size = Set(None);
    active.references = Set(None);
    active.deriver = Set(None);
    active
}

/// `ObjectMissing` demotes a reported path only after a storage probe found the object gone.
/// `Always` is for an invalidation, which wants the object gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoteWhen {
    ObjectMissing,
    Always,
}

fn probe_before_demoting(when: DemoteWhen, has_producer: bool) -> bool {
    when == DemoteWhen::ObjectMissing || !has_producer
}

crate::sql! {
    CLEAR_CACHE_AVAILABLE_TRUST = "UPDATE derivation_build SET cache_available = false \
             WHERE derivation = ANY($1) AND cache_available",
        params = [DerivationIds(64)];
}

/// Clear `cache_available` before the retire reads it, under
/// [`crate::graph::runtime_can_start::lock_cached_paths`] taken first. All writers follow the
/// class order `cached_path`, then `derivation_build`, or a concurrent retire deadlocks against
/// this path.
pub async fn demote_cached_output(
    ctx: &crate::DbContext,
    hash: &str,
    when: DemoteWhen,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    use gradient_entity::derivation_output::{Column as CDO, Entity as EDO};
    use sea_orm::{ActiveModelTrait, TransactionTrait};

    let db = &ctx.worker_db;
    let nar_storage = &ctx.storage.nar_storage;
    let outputs = EDO::find().filter(CDO::Hash.eq(hash)).all(db).await?;
    let has_producer = !outputs.is_empty();

    // Nothing is deleted on uncertainty. A present object stays, a probe error keeps it, and a
    // producerless input has nothing to rebuild it.
    if probe_before_demoting(when, has_producer) && nar_storage.exists(hash).await.unwrap_or(true) {
        return Ok(Vec::new());
    }

    let mut producers = Vec::with_capacity(outputs.len());
    for o in outputs {
        producers.push(o.derivation);
        demoted_output(o).update(db).await?;
    }

    let txn = db.begin().await?;
    crate::graph::runtime_can_start::lock_cached_paths(&txn, &[hash.to_owned()]).await?;
    let _shared_builds = crate::graph::can_start::lock_shared_builds(&txn, &producers).await?;
    if !producers.is_empty() {
        let ids: Vec<uuid::Uuid> = producers.iter().map(|d| d.into_inner()).collect();
        txn.execute_raw(CLEAR_CACHE_AVAILABLE_TRUST.bind([ids.into()]))
            .await?;
    }

    let mut retired =
        crate::graph::runtime_can_start::retire_outputs(&txn, &[hash.to_owned()]).await?;
    retired
        .transitions
        .extend(crate::graph::can_start::unpromote_ungated(&txn, &producers).await?);
    txn.commit().await?;

    let settled = crate::graph::can_start::update_and_settle_need(db, &producers).await?;
    retired.transitions.extend(settled.changes);
    crate::status::emit_transition_effects(ctx, &retired.transitions).await?;

    if let Err(e) = nar_storage.delete(hash).await {
        warn!(%hash, error = %e, "demote: failed to delete NAR object from storage");
    }

    Ok(producers)
}

/// The parents keep their cached outputs. Their next walk names the orphan producer below
/// them again.
pub async fn unwalk_parents_of(
    ctx: &crate::DbContext,
    missing_hash: &str,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    let parents = parents_of_hash(&ctx.worker_db, missing_hash).await?;
    let changes = crate::graph::can_start::unwalk_derivations(ctx, &parents).await?;
    crate::status::emit_transition_effects(ctx, &changes).await?;

    Ok(parents)
}

pub(crate) fn unbacked_trusted_outputs_select() -> String {
    format!(
        r#"
    SELECT DISTINCT o.hash
    FROM derivation_output o
    JOIN derivation_build db ON db.derivation = o.derivation
    WHERE db.status IN ({terminal_success})
      AND o.external_url IS NULL
      AND NOT EXISTS (
          SELECT 1 FROM cached_path cp
          WHERE cp.hash = o.hash AND cp.file_hash IS NOT NULL)
"#,
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
    )
}

crate::sql! {
    OUTPUT_PARENTS_SELECT = "SELECT DISTINCT e.derivation AS parent \
     FROM derivation_dependency e \
     WHERE e.kind IN (1, 2) \
       AND e.dependency IN (SELECT p.derivation FROM derivation_output p WHERE p.hash = $1)",
        params = [CachedPathHash];
}

async fn parents_of_hash<C: ConnectionTrait>(
    db: &C,
    hash: &str,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    use sea_orm::FromQueryResult;

    #[derive(sea_orm::FromQueryResult)]
    struct ParentRow {
        parent: uuid::Uuid,
    }

    Ok(
        ParentRow::find_by_statement(OUTPUT_PARENTS_SELECT.bind([hash.into()]))
            .all(db)
            .await?
            .into_iter()
            .map(|r| DerivationId::new(r.parent))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present_nar(hash: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nars").join(&hash[..2]);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{}.nar.zst", &hash[2..]));
        std::fs::write(&file, b"x").unwrap();
        (tmp, file)
    }

    #[tokio::test]
    async fn demote_preserves_a_present_producerless_object() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, file) = present_nar(hash);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::derivation_output::Model>::new()])
            .into_connection();
        let (ctx, _pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        let producers = demote_cached_output(&ctx, hash, DemoteWhen::ObjectMissing)
            .await
            .unwrap();

        assert!(producers.is_empty(), "a producerless input has no producer");
        assert!(
            file.exists(),
            "a present producerless object must be preserved"
        );
    }

    #[tokio::test]
    async fn an_invalidation_deletes_a_present_output_object() {
        use gradient_types::ids::{DerivationId, DerivationOutputId};
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
        use std::collections::BTreeMap;

        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, file) = present_nar(hash);

        let producer = DerivationId::now_v7();
        let output = gradient_entity::derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation: producer,
            hash: hash.to_string(),
            ..Default::default()
        };
        let producer_row =
            BTreeMap::from([("derivation".to_owned(), Value::from(producer.into_inner()))]);
        let deleted = BTreeMap::from([("hash".to_owned(), Value::from(hash.to_owned()))]);
        let none = Vec::<BTreeMap<String, Value>>::new();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![output.clone()], vec![output]])
            .append_query_results([
                vec![producer_row],
                none.clone(),
                vec![deleted],
                none.clone(),
                none.clone(),
                none.clone(),
                none.clone(),
                none,
            ])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                8
            ])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        let producers = demote_cached_output(&ctx, hash, DemoteWhen::Always)
            .await
            .unwrap();

        assert_eq!(producers.len(), 1, "the output's producer is returned");
        assert!(!file.exists(), "demote must delete the output's NAR object");
        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log.iter().any(|s| s.contains("DELETE FROM cached_path")),
            "the row must be retired, so the counters it backed move with it: {log:?}"
        );
        assert_eq!(
            log.len(),
            18,
            "outputs, demote, path lock, shared build lock, trust clear, retire lock, \
             producers of the hash, the complete closure they had, delete, is_cached, \
             shared build lock, mark, reset, owners, un-promote, and the raised, locked \
             update of what the producers now need: {log:?}"
        );
        let paths = log
            .iter()
            .position(|s| s.contains("FROM cached_path WHERE hash = ANY($1)"))
            .expect("the path lock is taken first");
        let shared_builds = log
            .iter()
            .position(|s| s.contains("FROM derivation_build WHERE derivation = ANY($1::uuid[])"))
            .expect("the shared build lock is taken");
        let trust = log
            .iter()
            .position(|s| s.contains("SET cache_available = false"))
            .expect("the upstream trust is dropped");
        let complete = log
            .iter()
            .position(|s| s.contains("ORDER BY db.derivation FOR NO KEY UPDATE"))
            .expect("the complete closure the producers had is read");
        let retire = log
            .iter()
            .position(|s| s.contains("DELETE FROM cached_path"))
            .expect("the row is retired");
        let mark = log
            .iter()
            .position(|s| s.contains("SET fetchable = false"))
            .expect("the producers are offered to the mark");
        let reset = log
            .iter()
            .position(|s| s.contains("substituted = false, attempt = 0"))
            .expect("the producer with nothing left to serve is reset");
        assert!(
            paths < trust,
            "this is the one path that writes derivation_build before a retire, so it takes the cached_path lock first or it deadlocks against a concurrent eviction: {log:?}"
        );
        assert!(
            paths < shared_builds && shared_builds < trust,
            "the trust clear names its producers in one UPDATE, so it acquires them in \
             plan order: the ordered shared build pass has to precede it or it deadlocks \
             against the can-start repair's chunk: {log:?}"
        );
        assert!(
            trust < retire,
            "the retire decides fetchability, so the stale offer must be gone first: {log:?}"
        );
        assert!(
            complete < retire,
            "which producers had a complete closure is the one endpoint the delete destroys, so it \
             is read under the lock before it: {log:?}"
        );
        assert!(
            mark < reset,
            "the producer reset belongs to the retire, which decides it from the \
             `fetchable` flag the mark has just written: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_reported_missing_output_stays_while_its_object_is_present() {
        use gradient_types::ids::{DerivationId, DerivationOutputId};
        use sea_orm::{DatabaseBackend, MockDatabase};

        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, file) = present_nar(hash);
        let output = gradient_entity::derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation: DerivationId::now_v7(),
            hash: hash.to_string(),
            is_cached: true,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![output]])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        let producers = demote_cached_output(&ctx, hash, DemoteWhen::ObjectMissing)
            .await
            .unwrap();

        assert!(
            producers.is_empty(),
            "nothing was demoted, so nothing is requeued"
        );
        assert!(
            file.exists(),
            "a worker's outdated view of the cache deletes no object"
        );
        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(
            log.len(),
            1,
            "the outputs are read and the probe ends it, before any output is touched: {log:?}"
        );
    }

    #[tokio::test]
    async fn demote_of_a_hash_with_no_row_still_resets_its_producer() {
        use gradient_types::ids::{DerivationId, DerivationOutputId};
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
        use std::collections::BTreeMap;

        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, _file) = present_nar(hash);
        let producer = DerivationId::now_v7();
        let output = gradient_entity::derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation: producer,
            hash: hash.to_string(),
            ..Default::default()
        };
        let drv_row =
            BTreeMap::from([("derivation".to_owned(), Value::from(producer.into_inner()))]);

        let none = Vec::<BTreeMap<String, Value>>::new();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![output.clone()], vec![output]])
            .append_query_results([
                vec![drv_row.clone()],
                none.clone(),
                none.clone(),
                vec![drv_row],
                none.clone(),
                none.clone(),
                none.clone(),
                none.clone(),
                none.clone(),
                none,
            ])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                9
            ])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        demote_cached_output(&ctx, hash, DemoteWhen::Always)
            .await
            .unwrap();

        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(
            log.len(),
            21,
            "outputs, demote, path lock, shared build lock, trust clear, retire lock, \
             producers of the hash, the complete closure they had, the delete that finds \
             nothing, shared build lock, mark, ripple, the raised, locked update below \
             what the mark flipped, reset, owners, un-promote, and the raised, locked \
             update of what the producers now need: {log:?}"
        );
        assert!(
            !log.iter()
                .any(|s| s.contains("WHERE is_cached AND hash = ANY($1)")),
            "nothing was deleted, so the retire's own clears never run: {log:?}"
        );
        assert!(
            log.iter()
                .any(|s| s.contains("FROM derivation_output o WHERE o.hash = ANY($1)")),
            "the producers of the asked-for hash are still resolved: {log:?}"
        );
        assert!(
            log.iter().any(|s| s.contains("SET fetchable = false")),
            "and offered to the mark, which decides from the predicate: {log:?}"
        );
        assert!(
            log.iter().any(|s| s.contains("AND NOT db.fetchable")),
            "so the producer with nothing left to serve is reset: {log:?}"
        );
    }

    #[test]
    fn demote_clears_upstream_availability() {
        use sea_orm::ActiveValue::Set;

        let o = gradient_entity::derivation_output::Model {
            is_cached: true,
            cached_path: Some(gradient_types::ids::CachedPathId::now_v7()),
            external_url: Some("https://cache.example/x.narinfo".to_string()),
            nar_hash: Some("sha256:aaa".to_string()),
            file_hash: Some("sha256:bbb".to_string()),
            file_size: Some(42),
            ..Default::default()
        };

        let am = demoted_output(o);
        assert_eq!(am.is_cached, Set(false));
        assert_eq!(am.cached_path, Set(None));
        assert_eq!(
            am.external_url,
            Set(None),
            "external_url must be cleared so no upstream offer survives the demote"
        );
        assert_eq!(am.nar_hash, Set(None));
        assert_eq!(am.file_hash, Set(None));
        assert_eq!(am.file_size, Set(None));
    }

    #[test]
    fn unbacked_trusted_select_matches_the_gate() {
        let sql = unbacked_trusted_outputs_select()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS);
        assert!(
            sql.contains(&format!("db.status IN ({terminal_success})")),
            "must mirror the gate's success states: {sql}"
        );
        assert!(
            !sql.contains("o.is_cached"),
            "must NOT gate on is_cached (it is false for the never-cached-output dead zone): {sql}"
        );
        assert!(
            sql.contains("o.external_url IS NULL"),
            "must skip upstream-served outputs: {sql}"
        );
        assert!(
            sql.contains("NOT EXISTS") && sql.contains("cp.file_hash IS NOT NULL"),
            "must require a missing backing NAR: {sql}"
        );
    }

    #[test]
    fn a_reported_missing_path_is_probed_and_an_invalidation_probes_only_producerless_paths() {
        assert!(probe_before_demoting(DemoteWhen::ObjectMissing, true));
        assert!(probe_before_demoting(DemoteWhen::ObjectMissing, false));
        assert!(
            !probe_before_demoting(DemoteWhen::Always, true),
            "an invalidated output is demoted so its producer rebuilds it"
        );
        assert!(
            probe_before_demoting(DemoteWhen::Always, false),
            "a producerless input has nothing to rebuild it, so a present copy is kept"
        );
    }

    #[test]
    fn output_parents_exclude_producerless_drv_and_source() {
        let sql = OUTPUT_PARENTS_SELECT
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            sql.contains("FROM derivation_dependency e") && sql.contains("e.kind IN (1, 2)"),
            "must resolve the runtime parents of the missing hash: {sql}"
        );
        assert!(
            sql.contains("SELECT DISTINCT e.derivation AS parent"),
            "the parents are derivations to walk again, not outputs to demote: {sql}"
        );
    }
}
