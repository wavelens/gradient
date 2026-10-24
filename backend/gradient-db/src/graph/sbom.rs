/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::entry_point::Licenses;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter};
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::graph::runtime_closure::parse_reference_hash;
use gradient_types::*;

#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct EntryPointOutput {
    pub attr: String,
    pub licenses: Licenses,
    pub hash: String,
    pub package: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationName {
    pub name: String,
    pub pname: Option<String>,
}

#[derive(FromQueryResult)]
struct NamedOutput {
    hash: String,
    name: String,
    pname: Option<String>,
}

crate::sql! {
    ENTRY_POINT_OUTPUTS = "SELECT ep.eval AS attr, ep.licenses, o.hash, o.package \
        FROM entry_point ep JOIN derivation_output o ON o.derivation = ep.derivation \
        WHERE ep.evaluation = $1 AND o.hash IS NOT NULL AND o.package IS NOT NULL",
        params = [EvaluationId];
    DERIVATION_NAMES = "SELECT DISTINCT ON (o.hash) o.hash, d.name, d.pname \
        FROM derivation_output o JOIN derivation d ON d.id = o.derivation \
        WHERE o.hash = ANY($1)",
        params = [CachedPathHashes(64)];
}

pub async fn entry_point_outputs<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<EntryPointOutput>, DbErr> {
    EntryPointOutput::find_by_statement(ENTRY_POINT_OUTPUTS.bind([evaluation.into_inner().into()]))
        .all(db)
        .await
}

pub async fn with_referenced_paths<C: ConnectionTrait>(
    db: &C,
    mut reached: HashMap<String, MCachedPath>,
) -> Result<HashMap<String, MCachedPath>, DbErr> {
    let mut absent: HashSet<String> = HashSet::new();
    loop {
        let missing: Vec<String> = missing_references(&reached)
            .into_iter()
            .filter(|hash| !absent.contains(hash))
            .collect();
        if missing.is_empty() {
            return Ok(reached);
        }

        let found = crate::fetch_in_chunks(&missing, |chunk| async move {
            ECachedPath::find()
                .filter(CCachedPath::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await?;
        let found: HashSet<String> = found
            .into_iter()
            .map(|path| {
                let hash = path.hash.clone();
                reached.insert(hash.clone(), path);
                hash
            })
            .collect();
        absent.extend(missing.into_iter().filter(|hash| !found.contains(hash)));
    }
}

pub fn missing_references(reached: &HashMap<String, MCachedPath>) -> Vec<String> {
    reached
        .values()
        .flat_map(|path| {
            path.references
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(parse_reference_hash)
        })
        .filter(|hash| !reached.contains_key(hash))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub async fn derivation_names<C: ConnectionTrait>(
    db: &C,
    output_hashes: &[String],
) -> Result<HashMap<String, DerivationName>, DbErr> {
    if output_hashes.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = crate::fetch_in_chunks(output_hashes, |chunk| async move {
        NamedOutput::find_by_statement(DERIVATION_NAMES.bind([chunk.into()]))
            .all(db)
            .await
    })
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.hash,
                DerivationName {
                    name: row.name,
                    pname: row.pname,
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::entry_point::License;
    use gradient_fixtures::mock::mock_db;
    use sea_orm::Value;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn entry_point_outputs_decode_the_stored_licenses() {
        let row = BTreeMap::from([
            (
                "attr".to_owned(),
                Value::from("packages.x86_64-linux.hello"),
            ),
            (
                "licenses".to_owned(),
                Value::from(serde_json::json!([{"id": "GPL-3.0-or-later"}, {"name": "unfree"}])),
            ),
            ("hash".to_owned(), Value::from("abc")),
            ("package".to_owned(), Value::from("hello-2.12.1")),
        ]);
        let db = mock_db()
            .append_query_results([vec![row]])
            .into_connection();

        let outputs = entry_point_outputs(&db, EvaluationId::now_v7())
            .await
            .unwrap();

        assert_eq!(
            outputs[0].licenses,
            Licenses(vec![
                License::Spdx {
                    id: "GPL-3.0-or-later".into()
                },
                License::Name {
                    name: "unfree".into()
                },
            ])
        );
    }

    fn path(hash: &str, references: &str) -> MCachedPath {
        MCachedPath {
            hash: hash.into(),
            package: format!("{hash}-pkg"),
            references: Some(references.into()),
            ..Default::default()
        }
    }

    #[test]
    fn only_references_outside_the_closure_are_missing() {
        let a = "0123456789abcdfghijklmnpqrsvwxyz";
        let b = "1123456789abcdfghijklmnpqrsvwxyz";
        let src = "2123456789abcdfghijklmnpqrsvwxyz";
        let reached = HashMap::from([
            (
                a.to_owned(),
                path(a, &format!("{a}-pkg {b}-pkg {src}-script.sh")),
            ),
            (b.to_owned(), path(b, "")),
        ]);

        assert_eq!(missing_references(&reached), vec![src.to_owned()]);
    }

    #[tokio::test]
    async fn derivation_names_are_keyed_by_output_hash() {
        let row = BTreeMap::from([
            ("hash".to_owned(), Value::from("abc")),
            ("name".to_owned(), Value::from("hello-2.12.1")),
            ("pname".to_owned(), Value::from("hello")),
        ]);
        let db = mock_db()
            .append_query_results([vec![row]])
            .into_connection();

        let names = derivation_names(&db, &["abc".to_owned()]).await.unwrap();

        assert_eq!(
            names.get("abc"),
            Some(&DerivationName {
                name: "hello-2.12.1".into(),
                pname: Some("hello".into()),
            })
        );
    }
}
