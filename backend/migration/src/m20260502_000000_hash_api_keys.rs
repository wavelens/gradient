/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Replace plaintext `api.key` values with their lowercase hex SHA-256 digest.
//!
//! After this migration the column stores a 64-char sha256 hex hash; the auth
//! lookup hashes the incoming token before querying. Existing tokens stay
//! valid because we hash in place - clients see no change.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};
use sha2::{Digest, Sha256};

#[derive(DeriveMigrationName)]
pub struct Migration;

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let bytes = h.finalize();
    let mut out = String::with_capacity(64);
    for b in bytes {
        write!(&mut out, "{:02x}", b).unwrap();
    }
    out
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();

        let rows = db
            .query_all(Statement::from_string(
                backend,
                "SELECT id, key FROM api WHERE length(key) <> 64 OR key !~ '^[0-9a-f]+$'"
                    .to_string(),
            ))
            .await?;

        for row in rows {
            let id: uuid::Uuid = row.try_get_by_index(0)?;
            let key: String = row.try_get_by_index(1)?;
            let hashed = sha256_hex(&key);
            db.execute(Statement::from_sql_and_values(
                backend,
                "UPDATE api SET key = $1 WHERE id = $2",
                [hashed.into(), id.into()],
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_known_vector() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
