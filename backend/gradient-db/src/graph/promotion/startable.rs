/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DbErr};

/// The dispatch gate reads the invariant: `Queued` means the gates held when the
/// shared build was promoted, and a regression un-promotes. Reachability still filters
/// shared builds left queued after their last referencing evaluation was torn down.
///
/// The open-`dispatched_job` arm is a dispatch gate, not a can-start state one, so it
/// lives here and not in `can_start::promote`: a shared build that is already out is
/// still perfectly promotable and must stay `Queued` for the report that closes
/// it. The whole startable set is read only by the dispatcher's startup and periodic
/// resync; between them it reads what moved, through [`find_startable_shared_builds_among`].
pub async fn find_startable_shared_builds<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<gradient_types::MDerivationBuild>, DbErr> {
    use sea_orm::EntityTrait;
    gradient_types::EDerivationBuild::find()
        .from_raw_sql(FIND_STARTABLE_SHARED_BUILDS.stmt())
        .all(db)
        .await
}

/// The same gate over the shared builds of `derivations` alone: the startable-set moves
/// one pass admits, so its cost follows what moved, not what is queued.
pub async fn find_startable_shared_builds_among<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<gradient_types::MDerivationBuild>, DbErr> {
    use sea_orm::EntityTrait;
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    gradient_types::EDerivationBuild::find()
        .from_raw_sql(FIND_STARTABLE_SHARED_BUILDS_AMONG.bind([ids.into()]))
        .all(db)
        .await
}

fn startable_shared_builds_sql(scope: &str) -> String {
    let not_in_flight = crate::scheduling::assignment_record::no_open_assignment_predicate(
        &crate::scheduling::assignment_record::build_job_key_sql("db.id"),
    );

    format!(
        r#"
        SELECT db.*
        FROM derivation_build db
        WHERE db.status = {queued}
          {scope}
          AND {not_in_flight}
          AND EXISTS (
            SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation)
        ORDER BY
            (SELECT count(*)
               FROM derivation_dependency dd
              WHERE dd.derivation = db.derivation) DESC,
            db.updated_at ASC
        "#,
        queued = crate::sql::status::build(BuildStatus::Queued),
    )
}

fn find_startable_shared_builds_sql() -> String {
    startable_shared_builds_sql("")
}

fn find_startable_shared_builds_among_sql() -> String {
    startable_shared_builds_sql("AND db.derivation = ANY($1::uuid[])")
}

crate::sql_fn! {
    FIND_STARTABLE_SHARED_BUILDS = find_startable_shared_builds_sql,
        params = [],
        tier = Bulk;

    FIND_STARTABLE_SHARED_BUILDS_AMONG = find_startable_shared_builds_among_sql,
        params = [DerivationIds(64)];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dispatch trusts the queue invariant: no can-start state term is re-evaluated
    /// per row here, only the status and the reachability check.
    #[test]
    fn assign_reads_the_queued_invariant_only() {
        let sql = find_startable_shared_builds_sql()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(sql.contains(&format!(
            "db.status = {}",
            crate::sql::status::build(BuildStatus::Queued)
        )));
        assert!(sql.contains("FROM build_job bj WHERE bj.derivation = db.derivation"));
        assert!(
            !sql.contains("blocking_deps")
                && !sql.contains("fetchable")
                && !sql.contains("cached_path"),
            "{sql}"
        );
    }

    /// The tracker's `untracked` filter is in memory and empty after a core
    /// respawn; the row is what survives, so the dispatch select carries the
    /// open-row gate itself.
    #[test]
    fn assign_refuses_a_shared_build_whose_assignment_row_is_open() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let gate = norm(
            crate::scheduling::assignment_record::no_open_assignment_predicate(
                &crate::scheduling::assignment_record::build_job_key_sql("db.id"),
            ),
        );

        assert!(norm(find_startable_shared_builds_sql()).contains(&gate));
    }

    /// The delta is the resync's gate narrowed to what moved: a copy that
    /// drifted would admit a shared build the resync then prunes, or the reverse.
    #[test]
    fn the_delta_is_the_startable_set_narrowed_to_what_moved() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let among = norm(find_startable_shared_builds_among_sql());
        let scope = "AND db.derivation = ANY($1::uuid[]) ";

        assert!(among.contains(scope), "{among}");
        assert_eq!(
            among.replacen(scope, "", 1),
            norm(find_startable_shared_builds_sql())
        );
    }

    #[tokio::test]
    async fn no_moves_means_no_statement() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();

        let shared_builds = find_startable_shared_builds_among(&db, &[])
            .await
            .expect("no-op");

        assert!(shared_builds.is_empty());
        assert!(db.into_transaction_log().is_empty());
    }
}
