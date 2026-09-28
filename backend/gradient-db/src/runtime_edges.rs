/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Runtime edges of the derivation graph. They are learned, never declared: from
//! an upstream narinfo when an output is probed, and from the NAR when it lands.
//! A reference whose hash has no producing derivation yet is either an `inputSrc`
//! the evaluation pushed itself or an output of a stub the walk has not reached,
//! so the walk that gives a stub its outputs adopts the references naming them.
//!
//! Two derivations that produce the same output paths (twins: `.drv`s equal modulo
//! their fixed-output inputs) never get an edge between them. Each one's reference to
//! the shared path is a reference to its own output; as an edge it made each twin's
//! wholeness wait on the other's, and neither could ever become fetchable.

use gradient_types::ids::DerivationId;
use sea_orm::{ConnectionTrait, DbErr, Value};

use crate::readiness::ids;

crate::sql! {
    /// One row per producer, add-only: an edge the walk already wrote as a build
    /// edge is upgraded to `Both` rather than duplicated, and a path that
    /// references its own producer's (or a twin's) output is not an edge at all.
    INSERT_RUNTIME_EDGES = r#"
INSERT INTO derivation_dependency (derivation, dependency, kind)
SELECT $1, d.dependency, 1 FROM unnest($2::uuid[]) AS d(dependency)
WHERE d.dependency <> $1
  AND NOT EXISTS (SELECT 1 FROM derivation_output own
                  JOIN derivation_output twin ON twin.hash = own.hash
                  WHERE own.derivation = $1 AND twin.derivation = d.dependency)
ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0
"#,
        params = [DerivationId, DerivationIds(64)];

    /// The runtime edges into `$1` that references recorded before `$1` had output
    /// rows could not resolve, from both places a reference is kept. Returns each
    /// referrer whose edge landed or was upgraded, once per edge.
    ///
    /// The producer lookup is fenced: a generic plan guesses thousands of referrers
    /// per GIN probe and hash-joins them against a sequential scan of every output.
    ADOPT_REFERENCED_OUTPUTS = r#"
INSERT INTO derivation_dependency (derivation, dependency, kind)
SELECT DISTINCT r.derivation, o.derivation, 1
FROM derivation_output o
CROSS JOIN LATERAL (VALUES (ARRAY[o.hash || '-' || o.package, '/nix/store/' || o.hash || '-' || o.package])) AS t(tokens)
JOIN LATERAL (
    SELECT cp.hash FROM cached_path cp WHERE string_to_array(cp."references", ' ') && t.tokens
    UNION
    SELECT ro.hash FROM derivation_output ro WHERE string_to_array(ro.references_list, ' ') && t.tokens
) h ON true
JOIN LATERAL (SELECT p.derivation FROM derivation_output p WHERE p.hash = h.hash OFFSET 0) r
  ON r.derivation <> o.derivation
WHERE o.derivation = ANY($1::uuid[])
  AND NOT EXISTS (SELECT 1 FROM derivation_output own
                  WHERE own.derivation = r.derivation AND own.hash = o.hash)
ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0
RETURNING derivation
"#,
        params = [DerivationIds(64)],
        tier = Bulk;
}

/// The store-path hash of a narinfo reference token, which the worker sends as
/// either a bare `hash-name` or a full `/nix/store/hash-name`.
pub(crate) fn hash_of_token(token: &str) -> Option<String> {
    let base = token.trim_start_matches("/nix/store/");
    let hash = base.split('-').next()?;
    gradient_util::nix_hash::is_nix32_hash(hash).then(|| hash.to_owned())
}

/// The derivations producing the outputs `tokens` names. A token whose hash has
/// no `derivation_output` row yields nothing, which is how an `inputSrc` drops out.
pub async fn producers_of_tokens<C: ConnectionTrait>(
    db: &C,
    tokens: &[String],
) -> Result<Vec<DerivationId>, DbErr> {
    let hashes: Vec<String> = tokens.iter().filter_map(|t| hash_of_token(t)).collect();

    crate::reachability::producers_of_hashes(db, &hashes).await
}

/// Write `producers` as runtime dependencies of `referrer`, returning the rows
/// inserted or upgraded to `Both`.
pub async fn insert_runtime_edges<C: ConnectionTrait>(
    db: &C,
    referrer: DerivationId,
    producers: &[DerivationId],
) -> Result<u64, DbErr> {
    if producers.is_empty() {
        return Ok(0);
    }

    Ok(db
        .execute_raw(
            INSERT_RUNTIME_EDGES.bind([Value::from(referrer.into_inner()), ids(producers)]),
        )
        .await?
        .rows_affected())
}

/// Write the runtime edges earlier references name into the outputs of `walked`,
/// returning the referrers whose runtime edges grew.
pub async fn adopt_referenced_outputs<C: ConnectionTrait>(
    db: &C,
    walked: &[DerivationId],
) -> Result<Vec<DerivationId>, DbErr> {
    if walked.is_empty() {
        return Ok(Vec::new());
    }

    let mut referrers: Vec<DerivationId> = db
        .query_all_raw(ADOPT_REFERENCED_OUTPUTS.bind([ids(walked)]))
        .await?
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect();
    referrers.sort_unstable();
    referrers.dedup();

    Ok(referrers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_insert_upgrades_a_build_edge_to_both_and_skips_self() {
        let sql = INSERT_RUNTIME_EDGES.text();
        assert!(
            sql.contains(
                "ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0"
            ),
            "{sql}"
        );
        assert!(sql.contains("WHERE d.dependency <> $1"), "{sql}");
    }

    #[test]
    fn neither_writer_links_a_derivation_to_a_twin_sharing_its_output() {
        let insert = INSERT_RUNTIME_EDGES.text();
        assert!(
            insert.contains("JOIN derivation_output twin ON twin.hash = own.hash")
                && insert.contains("WHERE own.derivation = $1 AND twin.derivation = d.dependency"),
            "{insert}"
        );
        let adopt = ADOPT_REFERENCED_OUTPUTS.text();
        assert!(
            adopt.contains("WHERE own.derivation = r.derivation AND own.hash = o.hash"),
            "{adopt}"
        );
    }

    #[test]
    fn adoption_matches_both_token_forms_against_both_reference_columns() {
        let sql = ADOPT_REFERENCED_OUTPUTS.text();
        assert!(
            sql.contains(
                "ARRAY[o.hash || '-' || o.package, '/nix/store/' || o.hash || '-' || o.package]"
            ),
            "{sql}"
        );
        assert!(
            sql.contains(r#"string_to_array(cp."references", ' ') && t.tokens"#),
            "{sql}"
        );
        assert!(
            sql.contains("string_to_array(ro.references_list, ' ') && t.tokens"),
            "{sql}"
        );
        assert!(
            sql.contains("WHERE p.hash = h.hash OFFSET 0"),
            "unfenced, the producer lookup seq-scans every output: {sql}"
        );
        assert!(
            sql.contains("SELECT DISTINCT r.derivation, o.derivation, 1"),
            "a referrer naming two outputs of one producer must land one row, or the upsert touches it twice: {sql}"
        );
    }

    #[tokio::test]
    async fn adoption_reports_each_grown_referrer_once() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        use std::collections::BTreeMap;

        let referrer = DerivationId::now_v7();
        let row = |id: DerivationId| {
            BTreeMap::from([("derivation".to_owned(), Value::from(id.into_inner()))])
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(referrer), row(referrer)]])
            .into_connection();

        let grown = adopt_referenced_outputs(&db, &[DerivationId::now_v7()])
            .await
            .unwrap();

        assert_eq!(grown, vec![referrer]);
    }

    #[tokio::test]
    async fn adopting_nothing_touches_the_database() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();

        assert!(adopt_referenced_outputs(&db, &[]).await.unwrap().is_empty());
        assert!(db.into_transaction_log().is_empty());
    }

    #[test]
    fn tokens_map_to_producers_through_the_output_hash() {
        assert_eq!(
            hash_of_token("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x"),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned())
        );
        assert_eq!(
            hash_of_token("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-y"),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned())
        );
        assert_eq!(hash_of_token("not-a-path"), None);
    }
}
