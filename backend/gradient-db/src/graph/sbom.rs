/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::entry_point::Licenses;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};
use std::collections::HashMap;

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
            ("attr".to_owned(), Value::from("packages.x86_64-linux.hello")),
            (
                "licenses".to_owned(),
                Value::from(serde_json::json!([{"id": "GPL-3.0-or-later"}, {"name": "unfree"}])),
            ),
            ("hash".to_owned(), Value::from("abc")),
            ("package".to_owned(), Value::from("hello-2.12.1")),
        ]);
        let db = mock_db().append_query_results([vec![row]]).into_connection();

        let outputs = entry_point_outputs(&db, EvaluationId::now_v7()).await.unwrap();

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

    #[tokio::test]
    async fn derivation_names_are_keyed_by_output_hash() {
        let row = BTreeMap::from([
            ("hash".to_owned(), Value::from("abc")),
            ("name".to_owned(), Value::from("hello-2.12.1")),
            ("pname".to_owned(), Value::from("hello")),
        ]);
        let db = mock_db().append_query_results([vec![row]]).into_connection();

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
