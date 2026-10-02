/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Twin derivations producing the same output paths must never get an edge between them.
//! Each twin's complete closure would wait on the other's.
//! Neither twin could then become fetchable.

use gradient_types::ids::DerivationId;
use sea_orm::{ConnectionTrait, DbErr, Value};

use crate::graph::can_start::ids;

crate::sql! {
    INSERT_RUNTIME_DEPENDENCIES = r#"
INSERT INTO derivation_dependency (derivation, dependency, kind)
SELECT $1, d.dependency, 1 FROM unnest($2::uuid[]) AS d(dependency)
WHERE d.dependency <> $1
  AND NOT EXISTS (SELECT 1 FROM derivation_output own
                  JOIN derivation_output twin ON twin.hash = own.hash
                  WHERE own.derivation = $1 AND twin.derivation = d.dependency)
ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0
"#,
        params = [DerivationId, DerivationIds(64)];

    /// The producer lookup is fenced.
    /// A generic plan is guessing thousands of parents per GIN probe.
    /// It would hash-join them against a sequential scan of every output.
    ADOPT_REFERENCED_OUTPUTS = r#"
INSERT INTO derivation_dependency (derivation, dependency, kind)
SELECT DISTINCT a.referrer, a.producer, 1
FROM (
    SELECT r.derivation AS referrer, o.derivation AS producer
    FROM derivation_output o
    CROSS JOIN LATERAL (VALUES (ARRAY[o.hash || '-' || o.package, '/nix/store/' || o.hash || '-' || o.package])) AS t(tokens)
    JOIN LATERAL (
        SELECT cp.hash FROM cached_path cp WHERE string_to_array(cp."references", ' ') && t.tokens
        UNION
        SELECT ro.hash FROM derivation_output ro WHERE string_to_array(ro.references_list, ' ') && t.tokens
    ) h ON true
    JOIN LATERAL (
        SELECT p.derivation FROM derivation_output p
        WHERE p.hash = h.hash
          AND NOT EXISTS (SELECT 1 FROM derivation_output own
                          WHERE own.derivation = p.derivation AND own.hash = o.hash)
        OFFSET 0) r
      ON r.derivation <> o.derivation
    WHERE o.derivation = ANY($1::uuid[])
    UNION ALL
    SELECT o.derivation, p.derivation
    FROM derivation_output o
    JOIN cached_path cp ON cp.hash = o.hash
    CROSS JOIN LATERAL (SELECT array_agg(tw.derivation) AS twins
                        FROM derivation_output tw WHERE tw.hash = o.hash) tw
    CROSS JOIN LATERAL unnest(string_to_array(cp."references", ' ')) AS t(token)
    JOIN derivation_output p
      ON p.hash = split_part(regexp_replace(t.token, '^/nix/store/', ''), '-', 1)
     AND p.derivation <> ALL(tw.twins)
    WHERE o.derivation = ANY($1::uuid[])
) a
ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0
RETURNING derivation
"#,
        params = [DerivationIds(64)],
        tier = Bulk;
}

pub(crate) fn hash_of_token(token: &str) -> Option<String> {
    let base = token.trim_start_matches("/nix/store/");
    let hash = base.split('-').next()?;
    gradient_util::nix_hash::is_nix32_hash(hash).then(|| hash.to_owned())
}

pub async fn producers_of_tokens<C: ConnectionTrait>(
    db: &C,
    tokens: &[String],
) -> Result<Vec<DerivationId>, DbErr> {
    let hashes: Vec<String> = tokens.iter().filter_map(|t| hash_of_token(t)).collect();

    crate::graph::reachability::producers_of_hashes(db, &hashes).await
}

pub async fn insert_runtime_dependencies<C: ConnectionTrait>(
    db: &C,
    parent: DerivationId,
    producers: &[DerivationId],
) -> Result<u64, DbErr> {
    if producers.is_empty() {
        return Ok(0);
    }

    Ok(db
        .execute_raw(
            INSERT_RUNTIME_DEPENDENCIES.bind([Value::from(parent.into_inner()), ids(producers)]),
        )
        .await?
        .rows_affected())
}

pub async fn adopt_referenced_outputs<C: ConnectionTrait>(
    db: &C,
    walked: &[DerivationId],
) -> Result<Vec<DerivationId>, DbErr> {
    if walked.is_empty() {
        return Ok(Vec::new());
    }

    let mut wanted_by: Vec<DerivationId> = db
        .query_all_raw(ADOPT_REFERENCED_OUTPUTS.bind([ids(walked)]))
        .await?
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect();
    wanted_by.sort_unstable();
    wanted_by.dedup();

    Ok(wanted_by)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_insert_upgrades_a_build_edge_to_both_and_skips_self() {
        let sql = INSERT_RUNTIME_DEPENDENCIES.text();
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
        let insert = INSERT_RUNTIME_DEPENDENCIES.text();
        assert!(
            insert.contains("JOIN derivation_output twin ON twin.hash = own.hash")
                && insert.contains("WHERE own.derivation = $1 AND twin.derivation = d.dependency"),
            "{insert}"
        );
        let adopt = ADOPT_REFERENCED_OUTPUTS.text();
        assert!(
            adopt.contains("WHERE own.derivation = p.derivation AND own.hash = o.hash"),
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
            sql.contains(
                "WHERE own.derivation = p.derivation AND own.hash = o.hash)\n        OFFSET 0) r"
            ),
            "unfenced, the producer lookup and its twin check seq-scan every output: {sql}"
        );
        assert!(
            sql.contains("SELECT DISTINCT a.referrer, a.producer, 1"),
            "a parent naming two outputs of one producer must land one row, or the upsert touches it twice: {sql}"
        );
    }

    #[test]
    fn a_walked_derivation_adopts_the_references_its_own_cached_nar_names() {
        let sql = ADOPT_REFERENCED_OUTPUTS.text();
        assert!(
            sql.contains(concat!(
                "    JOIN cached_path cp ON cp.hash = o.hash\n",
                "    CROSS JOIN LATERAL (SELECT array_agg(tw.derivation) AS twins\n",
                "                        FROM derivation_output tw WHERE tw.hash = o.hash) tw\n",
                "    CROSS JOIN LATERAL unnest(string_to_array(cp.\"references\", ' ')) AS t(token)\n",
                "    JOIN derivation_output p\n",
                "      ON p.hash = split_part(regexp_replace(t.token, '^/nix/store/', ''), '-', 1)\n",
                "     AND p.derivation <> ALL(tw.twins)\n",
                "    WHERE o.derivation = ANY($1::uuid[])",
            )),
            "a twin must be excluded once per output, not probed once per reference: {sql}"
        );
    }

    #[tokio::test]
    async fn adoption_reports_each_grown_parent_once() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        use std::collections::BTreeMap;

        let parent = DerivationId::now_v7();
        let row = |id: DerivationId| {
            BTreeMap::from([("derivation".to_owned(), Value::from(id.into_inner()))])
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(parent), row(parent)]])
            .into_connection();

        let grown = adopt_referenced_outputs(&db, &[DerivationId::now_v7()])
            .await
            .unwrap();

        assert_eq!(grown, vec![parent]);
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
