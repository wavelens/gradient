/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Runtime edges of the derivation graph. They are learned, never declared: from
//! an upstream narinfo when an output is probed, and from the NAR when it lands.
//! A reference whose hash has no producing derivation is an `inputSrc` the
//! evaluation pushed itself and gets no edge.

use gradient_types::ids::DerivationId;
use sea_orm::{ConnectionTrait, DbErr, Value};

use crate::readiness::ids;

crate::sql! {
    /// One row per producer, add-only: an edge the walk already wrote as a build
    /// edge is upgraded to `Both` rather than duplicated, and a path that
    /// references its own producer's other output is not an edge at all.
    INSERT_RUNTIME_EDGES = r#"
INSERT INTO derivation_dependency (derivation, dependency, kind)
SELECT $1, d.dependency, 1 FROM unnest($2::uuid[]) AS d(dependency) WHERE d.dependency <> $1
ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 0
"#,
        params = [DerivationId, DerivationIds(64)];
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
