/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! No edge may join twins producing the same output paths, and no edge may close a cycle with a
//! direct edge back. Real Nix references can form neither, and either edge would make two closures
//! wait on each other, so no build of them could ever become fetchable.

use gradient_types::ids::DerivationId;
use sea_orm::{ConnectionTrait, DbErr, Value};
use std::sync::LazyLock;

use crate::graph::can_start::ids;

/// A bare `NOT EXISTS` becomes an anti join, which the overestimated `&&` matches turn into a scan
/// of every edge. The scalar subquery stays one index probe per candidate pair.
fn no_edge_back(referrer: &str, producer: &str) -> String {
    format!(
        "(SELECT NOT EXISTS (SELECT 1 FROM derivation_dependency back \
         WHERE back.derivation = {producer} AND back.dependency = {referrer}))"
    )
}

static INSERT_RUNTIME_DEPENDENCIES_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"
INSERT INTO derivation_dependency (derivation, dependency, kind)
SELECT $1, d.dependency, 1 FROM unnest($2::uuid[]) AS d(dependency)
WHERE d.dependency <> $1
  AND NOT EXISTS (SELECT 1 FROM derivation_output own
                  JOIN derivation_output twin ON twin.hash = own.hash
                  WHERE own.derivation = $1 AND twin.derivation = d.dependency)
  AND {no_cycle}
ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0
"#,
        no_cycle = no_edge_back("$1", "d.dependency"),
    )
});

static ADOPT_REFERENCED_OUTPUTS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"
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
WHERE {no_cycle}
ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0
RETURNING derivation
"#,
        no_cycle = no_edge_back("a.referrer", "a.producer"),
    )
});

crate::sql_lazy! {
    INSERT_RUNTIME_DEPENDENCIES = || INSERT_RUNTIME_DEPENDENCIES_SQL.as_str(),
        params = [DerivationId, DerivationIds(64)];

    ADOPT_REFERENCED_OUTPUTS = || ADOPT_REFERENCED_OUTPUTS_SQL.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
}

pub(crate) fn hash_of_token(token: &str) -> Option<String> {
    let path = gradient_types::StorePath::parse_stored(token).ok()?;
    gradient_util::nix_hash::is_nix32_hash(path.hash()).then(|| path.hash().to_owned())
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
    use gradient_fixtures::mock::{empty_db, mock_db};

    fn drv_path(key: &str) -> String {
        format!("/nix/store/{}-runtime.drv", key.repeat(32))
    }

    #[tokio::test]
    async fn insert_runtime_dependencies_skips_self_twin_and_cycle_edges_and_upgrades_build_time_edge()
     {
        use gradient_entity::derivation_dependency::EdgeKind;

        let [build_input, producer, twin, parent, cyclic] = ["1", "2", "3", "4", "5"];
        let hash = |key: &str| key.repeat(32);
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_types::EvaluationId::now_v7())
            .derivation(&drv_path(build_input))
            .output(&hash(build_input))
            .derivation(&drv_path(producer))
            .output(&hash(producer))
            .derivation(&drv_path(twin))
            .output(&hash(parent))
            .derivation(&drv_path(parent))
            .output(&hash(parent))
            .depends_on(&drv_path(build_input))
            .derivation(&drv_path(cyclic))
            .output(&hash(cyclic))
            .depends_on(&drv_path(parent))
            .into_database()
            .await;
        let mut ids = Vec::new();
        for key in [parent, producer, twin, cyclic, build_input] {
            ids.push(db.id_of(&drv_path(key)).await);
        }

        let written = insert_runtime_dependencies(&db.connection(), ids[0], &ids)
            .await
            .unwrap();
        let mut edges = Vec::new();
        for key in [parent, producer, twin, cyclic, build_input] {
            edges.push(
                db.edge(&drv_path(parent), &drv_path(key))
                    .await
                    .map(|edge| edge.kind),
            );
        }
        db.finish().await;

        assert_eq!(written, 2);
        assert_eq!(
            edges,
            [
                None,
                Some(EdgeKind::Runtime),
                None,
                None,
                Some(EdgeKind::Both)
            ]
        );
    }

    #[tokio::test]
    async fn adopt_referenced_outputs_links_referrers_for_stored_or_recorded_reference_token_forms_skips_twins_and_creates_one_edge_for_two_named_producer_outputs()
     {
        use gradient_entity::derivation_dependency::EdgeKind;

        let [
            producer,
            stored_plain,
            stored_full,
            recorded_plain,
            recorded_full,
        ] = ["1", "2", "3", "4", "5"];
        let [twin, unrelated, walked, own_input] = ["6", "7", "8", "9"];
        let hash = |key: &str| key.repeat(32);
        let twin_side_hash = "a".repeat(32);
        let dev_hash = "b".repeat(32);
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_types::EvaluationId::now_v7())
            .derivation(&drv_path(producer))
            .output(&hash(producer))
            .named_output(&dev_hash, "dev")
            .derivation(&drv_path(stored_plain))
            .output(&hash(stored_plain))
            .cached_path(&hash(stored_plain), "stored-plain")
            .derivation(&drv_path(stored_full))
            .output(&hash(stored_full))
            .cached_path(&hash(stored_full), "stored-full")
            .derivation(&drv_path(recorded_plain))
            .output(&hash(recorded_plain))
            .derivation(&drv_path(recorded_full))
            .output(&hash(recorded_full))
            .derivation(&drv_path(twin))
            .output(&hash(producer))
            .named_output(&twin_side_hash, "dev")
            .cached_path(&twin_side_hash, "twin-side")
            .derivation(&drv_path(unrelated))
            .output(&hash(unrelated))
            .cached_path(&hash(unrelated), "unrelated")
            .derivation(&drv_path(own_input))
            .output(&hash(own_input))
            .derivation(&drv_path(walked))
            .output(&hash(walked))
            .cached_path(&hash(walked), "walked")
            .into_database()
            .await;
        let token = async |name: &str| {
            let output = db.output_of(&drv_path(producer), name).await;
            format!(
                "{}-{}",
                output.hash.expect("a hash"),
                output.package.expect("a package")
            )
        };
        let (out, dev) = (token("out").await, token("dev").await);
        let cached = |hash: String, references: String| {
            format!("UPDATE cached_path SET \"references\" = '{references}' WHERE hash = '{hash}';")
        };
        let recorded = |hash: String, references: String| {
            format!(
                "UPDATE derivation_output SET references_list = '{references}' WHERE hash = '{hash}';"
            )
        };
        db.execute(
            &[
                cached(hash(stored_plain), format!("{out} /nix/store/{dev}")),
                cached(hash(stored_full), format!("/nix/store/{out}")),
                recorded(hash(recorded_plain), out.clone()),
                recorded(hash(recorded_full), format!("/nix/store/{out}")),
                cached(twin_side_hash.clone(), out.clone()),
                cached(hash(unrelated), format!("{}-elsewhere", hash("c"))),
                cached(
                    hash(walked),
                    format!("{}-walked {}-input", hash(walked), hash(own_input)),
                ),
            ]
            .concat(),
        )
        .await;
        let ids = [
            db.id_of(&drv_path(producer)).await,
            db.id_of(&drv_path(walked)).await,
        ];

        let adopted = adopt_referenced_outputs(&db.connection(), &ids)
            .await
            .unwrap();
        let mut edges = Vec::new();
        for (from, to) in [
            (stored_plain, producer),
            (stored_full, producer),
            (recorded_plain, producer),
            (recorded_full, producer),
            (twin, producer),
            (unrelated, producer),
            (walked, own_input),
            (walked, walked),
        ] {
            edges.push(
                db.edge(&drv_path(from), &drv_path(to))
                    .await
                    .map(|edge| edge.kind),
            );
        }
        let mut expected_adopted = Vec::new();
        for key in [
            stored_plain,
            stored_full,
            recorded_plain,
            recorded_full,
            walked,
        ] {
            expected_adopted.push(db.id_of(&drv_path(key)).await);
        }
        expected_adopted.sort_unstable();
        db.finish().await;

        assert_eq!(adopted, expected_adopted);
        let linked = Some(EdgeKind::Runtime);
        assert_eq!(
            edges,
            [linked, linked, linked, linked, None, None, linked, None]
        );
    }

    #[tokio::test]
    async fn adoption_reports_each_grown_parent_once() {
        use std::collections::BTreeMap;

        let parent = DerivationId::now_v7();
        let row = |id: DerivationId| {
            BTreeMap::from([("derivation".to_owned(), Value::from(id.into_inner()))])
        };
        let db = mock_db()
            .append_query_results([vec![row(parent), row(parent)]])
            .into_connection();

        let grown = adopt_referenced_outputs(&db, &[DerivationId::now_v7()])
            .await
            .unwrap();

        assert_eq!(grown, vec![parent]);
    }

    #[tokio::test]
    async fn adopting_nothing_touches_the_database() {
        let db = empty_db();

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
