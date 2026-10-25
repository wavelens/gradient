/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![allow(dead_code, reason = "the evaluation starts syncing in the next commit")]

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use gradient_storage::eval_cache_segment::{
    ATTR_PLACEHOLDER, EvalCacheRow, Merge, SEGMENT_MAX_BYTES, encode, merge,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tracing::warn;

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

const SCHEMA: &str = "
create table if not exists Attributes (parent integer not null, name text, type integer not null, value text, context text, primary key (parent, name));
create table if not exists gradient_changed_attribute (attribute integer primary key);
create table if not exists gradient_applying (one integer);
create table if not exists gradient_applied_segment (key text primary key);
create trigger if not exists gradient_record_inserted_attribute after insert on Attributes
    when not exists (select 1 from gradient_applying)
    begin insert or ignore into gradient_changed_attribute (attribute) values (new.rowid); end;
create trigger if not exists gradient_record_updated_attribute after update on Attributes
    when not exists (select 1 from gradient_applying)
    begin insert or ignore into gradient_changed_attribute (attribute) values (new.rowid); end;
";

// Each changed row walks up its parents; a chain that stops before the root `(0, '')` is an orphan.
const CHANGED_ROWS: &str = "
with recursive chain (attribute, parent, name, type, value, context, depth) as (
    select a.rowid, a.parent, a.name, a.type, a.value, a.context, 0
    from gradient_changed_attribute c join Attributes a on a.rowid = c.attribute
    union all
    select chain.attribute, p.parent, p.name, null, null, null, chain.depth + 1
    from chain join Attributes p on p.rowid = chain.parent
    where chain.parent != 0 and chain.depth < 1024
)
select attribute, parent, name, type, value, context from chain order by attribute, depth
";

pub struct EvalCacheFile {
    conn: Connection,
}

pub fn open(path: &Path) -> Result<EvalCacheFile> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create eval cache directory {}", parent.display()))?;
    }

    let conn =
        Connection::open(path).with_context(|| format!("open eval cache {}", path.display()))?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update_and_check(None, "journal_mode", "wal", |row| row.get::<_, String>(0))?;
    conn.execute_batch(SCHEMA)
        .context("create the eval cache change log")?;
    Ok(EvalCacheFile { conn })
}

struct Link {
    attribute: i64,
    parent: i64,
    name: String,
    kind: Option<i64>,
    value: Option<String>,
    context: Option<String>,
}

impl EvalCacheFile {
    pub fn export_changes(&mut self) -> Result<Vec<Vec<u8>>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = changed_rows(&tx)?;
        let segments = segments_of(rows)?;
        tx.execute("delete from gradient_changed_attribute", [])?;
        tx.commit()?;
        Ok(segments)
    }

    pub fn apply(&mut self, key: &str, rows: &[EvalCacheRow]) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("insert into gradient_applying (one) values (1)", [])?;
        let mut rowids = HashMap::new();
        for row in rows {
            apply_row(&tx, &mut rowids, row)?;
        }

        tx.execute(
            "insert or ignore into gradient_applied_segment (key) values (?1)",
            [key],
        )?;
        tx.execute("delete from gradient_applying", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn applied_keys(&self) -> Result<HashSet<String>> {
        let mut statement = self
            .conn
            .prepare("select key from gradient_applied_segment")?;
        let keys = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        Ok(keys)
    }

    pub fn forget_keys_except(&mut self, listed: &[String]) -> Result<()> {
        let listed: HashSet<&str> = listed.iter().map(String::as_str).collect();
        let stale: Vec<String> = self
            .applied_keys()?
            .into_iter()
            .filter(|key| !listed.contains(key.as_str()))
            .collect();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for key in &stale {
            tx.execute("delete from gradient_applied_segment where key = ?1", [key])?;
        }

        tx.commit()?;
        Ok(())
    }
}

fn changed_rows(tx: &Transaction<'_>) -> Result<Vec<EvalCacheRow>> {
    let mut statement = tx.prepare(CHANGED_ROWS)?;
    let links = statement
        .query_map([], |row| {
            Ok(Link {
                attribute: row.get(0)?,
                parent: row.get(1)?,
                name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                kind: row.get(3)?,
                value: row.get(4)?,
                context: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<Link>>>()?;

    Ok(links
        .chunk_by(|a, b| a.attribute == b.attribute)
        .filter_map(row_of_chain)
        .collect())
}

fn row_of_chain(chain: &[Link]) -> Option<EvalCacheRow> {
    let (root, below_root) = chain.split_last()?;
    if root.parent != 0 {
        return None;
    }

    let changed = chain.first()?;
    Some(EvalCacheRow {
        path: below_root
            .iter()
            .rev()
            .map(|link| link.name.clone())
            .collect(),
        kind: u8::try_from(changed.kind?).ok()?,
        value: changed.value.clone(),
        context: changed.context.clone(),
    })
}

fn estimated_size(row: &EvalCacheRow) -> usize {
    let text = |part: &Option<String>| part.as_ref().map_or(0, String::len) + 16;
    32 + row.path.iter().map(|part| part.len() + 16).sum::<usize>()
        + text(&row.value)
        + text(&row.context)
}

fn segments_of(rows: Vec<EvalCacheRow>) -> Result<Vec<Vec<u8>>> {
    let mut segments = Vec::new();
    let mut chunk = Vec::new();
    let mut chunk_size = 0;
    for row in rows {
        let size = estimated_size(&row);
        if !chunk.is_empty() && chunk_size + size > SEGMENT_MAX_BYTES {
            encode_within_limit(&std::mem::take(&mut chunk), &mut segments)?;
            chunk_size = 0;
        }

        chunk_size += size;
        chunk.push(row);
    }

    if !chunk.is_empty() {
        encode_within_limit(&chunk, &mut segments)?;
    }

    Ok(segments)
}

fn encode_within_limit(rows: &[EvalCacheRow], segments: &mut Vec<Vec<u8>>) -> Result<()> {
    let bytes = encode(rows)?;
    if bytes.len() <= SEGMENT_MAX_BYTES {
        segments.push(bytes);
        return Ok(());
    }

    if let [row] = rows {
        warn!(path = ?row.path, "eval cache row is larger than a segment; keeping it local");
        return Ok(());
    }

    let (first, second) = rows.split_at(rows.len() / 2);
    encode_within_limit(first, segments)?;
    encode_within_limit(second, segments)
}

fn apply_row(
    tx: &Transaction<'_>,
    rowids: &mut HashMap<Vec<String>, i64>,
    row: &EvalCacheRow,
) -> Result<()> {
    let (parent, name) = match row.path.split_last() {
        Some((name, ancestors)) => (resolve(tx, rowids, ancestors)?, name.as_str()),
        None => (0, ""),
    };

    let existing = tx
        .prepare_cached(
            "select type, value, context from Attributes where parent = ?1 and name = ?2",
        )?
        .query_row(params![parent, name], |found| {
            Ok(EvalCacheRow {
                path: row.path.clone(),
                kind: u8::try_from(found.get::<_, i64>(0)?).unwrap_or(ATTR_PLACEHOLDER),
                value: found.get(1)?,
                context: found.get(2)?,
            })
        })
        .optional()?;
    if existing.is_some_and(|existing| merge(&existing, row) == Merge::Keep) {
        return Ok(());
    }

    tx.prepare_cached(
        "insert into Attributes (parent, name, type, value, context) values (?1, ?2, ?3, ?4, ?5) \
         on conflict (parent, name) do update set type = excluded.type, value = excluded.value, context = excluded.context",
    )?
    .execute(params![parent, name, row.kind, row.value, row.context])?;
    Ok(())
}

fn resolve(
    tx: &Transaction<'_>,
    rowids: &mut HashMap<Vec<String>, i64>,
    path: &[String],
) -> Result<i64> {
    if let Some(&rowid) = rowids.get(path) {
        return Ok(rowid);
    }

    let (parent, name) = match path.split_last() {
        Some((name, ancestors)) => (resolve(tx, rowids, ancestors)?, name.as_str()),
        None => (0, ""),
    };
    let rowid = rowid_or_placeholder(tx, parent, name)?;
    rowids.insert(path.to_vec(), rowid);
    Ok(rowid)
}

fn rowid_or_placeholder(tx: &Transaction<'_>, parent: i64, name: &str) -> Result<i64> {
    let found = tx
        .prepare_cached("select rowid from Attributes where parent = ?1 and name = ?2")?
        .query_row(params![parent, name], |row| row.get(0))
        .optional()?;
    if let Some(rowid) = found {
        return Ok(rowid);
    }

    tx.prepare_cached("insert into Attributes (parent, name, type) values (?1, ?2, ?3)")?
        .execute(params![parent, name, ATTR_PLACEHOLDER])?;
    Ok(tx.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_storage::eval_cache_segment::{ATTR_FULL_ATTRS, decode};
    use std::path::PathBuf;
    use std::time::Instant;

    const ATTR_STRING: u8 = 2;

    fn cache_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("eval-cache-v6").join("fp.sqlite")
    }

    fn nix(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.busy_timeout(BUSY_TIMEOUT).unwrap();
        conn
    }

    fn nix_insert(
        conn: &Connection,
        parent: i64,
        name: &str,
        kind: u8,
        value: Option<&str>,
    ) -> i64 {
        conn.execute(
            "insert or replace into Attributes(parent, name, type, value) values (?, ?, ?, ?)",
            params![parent, name, kind, value],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn nix_seal(conn: &Connection, parent: i64, name: &str) -> i64 {
        conn.query_row(
            "insert into Attributes(parent, name, type, value) values (?, ?, ?, ?) \
             on conflict(parent, name) do update set type = excluded.type, value = excluded.value \
             returning rowid",
            params![parent, name, ATTR_FULL_ATTRS, None::<String>],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn row(path: &[&str], kind: u8, value: Option<&str>) -> EvalCacheRow {
        EvalCacheRow {
            path: path.iter().map(|part| (*part).to_owned()).collect(),
            kind,
            value: value.map(str::to_owned),
            context: None,
        }
    }

    fn exported_rows(file: &mut EvalCacheFile) -> Vec<EvalCacheRow> {
        let mut rows: Vec<EvalCacheRow> = file
            .export_changes()
            .unwrap()
            .iter()
            .flat_map(|segment| decode(segment).unwrap())
            .collect();
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        rows
    }

    fn lookup(conn: &Connection, path: &[&str]) -> Option<(u8, Option<String>)> {
        let mut parent = 0;
        let mut found = None;
        for name in std::iter::once("").chain(path.iter().copied()) {
            let (rowid, kind, value): (i64, u8, Option<String>) = conn
                .query_row(
                    "select rowid, type, value from Attributes where parent = ?1 and name = ?2",
                    params![parent, name],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .unwrap()?;
            parent = rowid;
            found = Some((kind, value));
        }

        found
    }

    #[test]
    fn a_nix_write_is_exported_once_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(&dir);
        let mut file = open(&path).unwrap();
        let nix = nix(&path);
        let root = nix_seal(&nix, 0, "");
        nix_insert(&nix, root, "hello", ATTR_STRING, Some("world"));

        let rows = exported_rows(&mut file);

        assert_eq!(
            rows,
            vec![
                row(&[], ATTR_FULL_ATTRS, None),
                row(&["hello"], ATTR_STRING, Some("world")),
            ]
        );
        assert!(file.export_changes().unwrap().is_empty());
    }

    #[test]
    fn applied_rows_are_not_exported_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = open(&cache_path(&dir)).unwrap();

        file.apply(
            "k",
            &[
                row(&[], ATTR_FULL_ATTRS, None),
                row(&["hello"], ATTR_STRING, Some("world")),
            ],
        )
        .unwrap();

        assert!(file.export_changes().unwrap().is_empty());
        assert_eq!(
            file.applied_keys().unwrap(),
            HashSet::from(["k".to_owned()])
        );
    }

    #[test]
    fn apply_keeps_children_attached_when_a_placeholder_is_sealed() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(&dir);
        let mut file = open(&path).unwrap();

        file.apply(
            "k1",
            &[
                row(&["a"], ATTR_PLACEHOLDER, None),
                row(&["a", "b"], ATTR_STRING, Some("x")),
            ],
        )
        .unwrap();
        file.apply("k2", &[row(&["a"], ATTR_FULL_ATTRS, None)])
            .unwrap();

        let nix = nix(&path);
        assert_eq!(lookup(&nix, &["a"]), Some((ATTR_FULL_ATTRS, None)));
        assert_eq!(
            lookup(&nix, &["a", "b"]),
            Some((ATTR_STRING, Some("x".to_owned())))
        );
    }

    #[test]
    fn export_skips_rows_whose_parent_was_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(&dir);
        let mut file = open(&path).unwrap();
        let nix = nix(&path);
        let root = nix_seal(&nix, 0, "");
        let a = nix_insert(&nix, root, "a", ATTR_PLACEHOLDER, None);
        nix_insert(&nix, a, "b", ATTR_STRING, Some("x"));
        nix_insert(&nix, root, "a", ATTR_STRING, Some("y"));

        let rows = exported_rows(&mut file);

        assert_eq!(
            rows,
            vec![
                row(&[], ATTR_FULL_ATTRS, None),
                row(&["a"], ATTR_STRING, Some("y")),
            ]
        );
    }

    #[test]
    fn apply_waits_for_a_concurrent_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(&dir);
        let mut file = open(&path).unwrap();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            let mut nix = nix(&writer_path);
            let tx = nix
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute(
                "insert into Attributes(parent, name, type, value) values (0, '', ?1, null)",
                [ATTR_FULL_ATTRS],
            )
            .unwrap();
            locked_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            tx.commit().unwrap();
        });
        locked_rx.recv().unwrap();
        let started = Instant::now();

        file.apply("k", &[row(&["a"], ATTR_STRING, Some("x"))])
            .unwrap();

        assert!(started.elapsed() >= Duration::from_millis(100));
        writer.join().unwrap();
        assert!(file.applied_keys().unwrap().contains("k"));
        assert_eq!(
            lookup(&nix(&path), &["a"]),
            Some((ATTR_STRING, Some("x".to_owned())))
        );
    }

    #[test]
    fn export_splits_at_the_segment_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(&dir);
        let mut file = open(&path).unwrap();
        let nix = nix(&path);
        let root = nix_seal(&nix, 0, "");
        let value = "x".repeat(1024 * 1024);
        for child in 0..9 {
            nix_insert(&nix, root, &format!("c{child}"), ATTR_STRING, Some(&value));
        }

        let segments = file.export_changes().unwrap();

        assert_eq!(segments.len(), 2);
        assert!(segments.iter().all(|s| s.len() <= SEGMENT_MAX_BYTES));
        let rows: usize = segments.iter().map(|s| decode(s).unwrap().len()).sum();
        assert_eq!(rows, 10);
    }
}
