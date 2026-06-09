/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Convert legacy `sha256:<hex>` values in hash columns to the canonical
//! `sha256:<nix32>` form so URL hashes embedded in narinfo `URL:` fields
//! match the persisted `file_hash` / `nar_hash` columns directly.
//!
//! Affects:
//! - `derivation_output.file_hash`
//! - `cached_path.file_hash`
//! - `cached_path.nar_hash`
//!
//! New writes go through `gradient_core::nix_hash::normalize_nar_hash` at the
//! proto/scheduler layer; this migration only normalises pre-existing rows.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

fn nix32_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let len = (bytes.len() * 8 - 1) / 5 + 1;
    let mut out = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let byte0 = bytes.get(i).copied().unwrap_or(0) as u32;
        let byte1 = bytes.get(i + 1).copied().unwrap_or(0) as u32;
        let c = ((byte0 >> j) | (byte1 << (8 - j))) & 0x1f;
        out.push(CHARS[c as usize] as char);
    }
    out
}

/// Returns `Some(canonical)` when `value` is `sha256:<64-hex>`, else `None`.
fn hex_to_nix32(value: &str) -> Option<String> {
    let rest = value.strip_prefix("sha256:")?;
    if rest.len() != 64 || !rest.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: [u8; 32] = (0..32)
        .map(|i| u8::from_str_radix(&rest[i * 2..i * 2 + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()?
        .try_into()
        .ok()?;
    Some(format!("sha256:{}", nix32_encode(&bytes)))
}

async fn convert_column(
    db: &SchemaManagerConnection<'_>,
    table: &str,
    column: &str,
) -> Result<(), DbErr> {
    let backend = db.get_database_backend();
    let select = format!(
        "SELECT id, {column} FROM {table} \
         WHERE {column} IS NOT NULL \
           AND {column} ~ '^sha256:[0-9a-f]{{64}}$'"
    );
    let rows = db
        .query_all(Statement::from_string(backend, select))
        .await?;

    for row in rows {
        let id: uuid::Uuid = row.try_get_by_index(0)?;
        let value: String = row.try_get_by_index(1)?;
        let Some(canonical) = hex_to_nix32(&value) else {
            continue;
        };
        let update = format!("UPDATE {table} SET {column} = $1 WHERE id = $2");
        db.execute(Statement::from_sql_and_values(
            backend,
            update,
            [canonical.into(), id.into()],
        ))
        .await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        convert_column(db, "derivation_output", "file_hash").await?;
        convert_column(db, "cached_path", "file_hash").await?;
        convert_column(db, "cached_path", "nar_hash").await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // hex and nix32 are equivalent encodings of the same 32 bytes; the
        // read path normalises either form, so no reversal is needed.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_HEX: &str =
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const EMPTY_NIX32: &str = "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";

    #[test]
    fn converts_known_hex() {
        assert_eq!(hex_to_nix32(EMPTY_HEX).as_deref(), Some(EMPTY_NIX32));
    }

    #[test]
    fn ignores_already_nix32() {
        assert_eq!(hex_to_nix32(EMPTY_NIX32), None);
    }

    #[test]
    fn ignores_unprefixed() {
        assert_eq!(
            hex_to_nix32("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            None
        );
    }

    #[test]
    fn ignores_wrong_length() {
        assert_eq!(hex_to_nix32("sha256:abc"), None);
    }
}
