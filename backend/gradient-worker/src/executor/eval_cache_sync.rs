/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use gradient_storage::eval_cache_segment::{
    ATTR_FULL_ATTRS, ATTR_MISSING, ATTR_PLACEHOLDER, EvalCacheRow, Merge, child_listing,
    encode_bounded, listed_children, merge,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

const SCHEMA: &str = "
create table if not exists Attributes (parent integer not null, name text, type integer not null, value text, context text, primary key (parent, name));
create table if not exists gradient_changed_attribute (attribute integer primary key);
create table if not exists gradient_applying (one integer);
create table if not exists gradient_applied_segment (key text primary key);
create table if not exists gradient_unsent_segment (id integer primary key, bytes blob not null);
drop trigger if exists gradient_record_inserted_attribute;
drop trigger if exists gradient_record_updated_attribute;
create trigger gradient_record_inserted_attribute after insert on Attributes
    when not exists (select 1 from gradient_applying)
    begin insert into gradient_changed_attribute (attribute) select new.rowid
        where not exists (select 1 from gradient_changed_attribute where attribute = new.rowid); end;
create trigger gradient_record_updated_attribute after update on Attributes
    when not exists (select 1 from gradient_applying)
    begin insert into gradient_changed_attribute (attribute) select new.rowid
        where not exists (select 1 from gradient_changed_attribute where attribute = new.rowid); end;
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

pub fn is_busy(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<rusqlite::Error>()
        .and_then(rusqlite::Error::sqlite_error_code)
        == Some(rusqlite::ErrorCode::DatabaseBusy)
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
    pub fn export_changes(&mut self) -> Result<Vec<(i64, Vec<u8>)>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = changed_rows(&tx)?;
        tx.execute("delete from gradient_changed_attribute", [])?;
        let mut unsent = Vec::new();
        for segment in encode_bounded(&rows)? {
            tx.prepare_cached("insert into gradient_unsent_segment (bytes) values (?1)")?
                .execute([&segment])?;
            unsent.push((tx.last_insert_rowid(), segment));
        }

        tx.commit()?;
        Ok(unsent)
    }

    pub fn unsent_segments(&self) -> Result<Vec<(i64, Vec<u8>)>> {
        let mut statement = self
            .conn
            .prepare("select id, bytes from gradient_unsent_segment order by id")?;
        let unsent = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<(i64, Vec<u8>)>>>()?;
        Ok(unsent)
    }

    pub fn mark_sent(&mut self, id: i64) -> Result<()> {
        self.conn
            .execute("delete from gradient_unsent_segment where id = ?1", [id])?;
        Ok(())
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

    #[cfg(test)]
    pub(crate) fn set_busy_timeout(&self, timeout: Duration) {
        self.conn.busy_timeout(timeout).unwrap();
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

    let mut rows = Vec::new();
    for chain in links.chunk_by(|a, b| a.attribute == b.attribute) {
        let Some(mut row) = row_of_chain(chain) else {
            continue;
        };

        if row.kind == ATTR_FULL_ATTRS {
            row.value = Some(child_listing(present_children(tx, chain[0].attribute)?));
        }

        rows.push(row);
    }

    Ok(rows)
}

fn present_children(tx: &Transaction<'_>, attribute: i64) -> Result<Vec<String>> {
    let names = tx
        .prepare_cached("select name from Attributes where parent = ?1 and type != ?2")?
        .query_map(params![attribute, ATTR_MISSING], |row| {
            Ok(row.get::<_, Option<String>>(0)?.unwrap_or_default())
        })?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(names)
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

fn apply_row(
    tx: &Transaction<'_>,
    rowids: &mut HashMap<Vec<String>, i64>,
    row: &EvalCacheRow,
) -> Result<()> {
    let (parent, name) = match row.path.split_last() {
        Some((name, ancestors)) => (resolve(tx, rowids, ancestors)?, name.as_str()),
        None => (0, ""),
    };

    if row.kind == ATTR_MISSING && is_unlisted(tx, parent, name)? {
        return Ok(());
    }

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

    if row.kind == ATTR_FULL_ATTRS {
        return seal(tx, parent, name, &listed_children(row.value.as_deref())?);
    }

    tx.prepare_cached(
        "insert into Attributes (parent, name, type, value, context) values (?1, ?2, ?3, ?4, ?5) \
         on conflict (parent, name) do update set type = excluded.type, value = excluded.value, context = excluded.context",
    )?
    .execute(params![parent, name, row.kind, row.value, row.context])?;
    Ok(())
}

fn is_unlisted(tx: &Transaction<'_>, parent: i64, name: &str) -> Result<bool> {
    let unlisted = tx
        .prepare_cached(
            "select exists (select 1 from Attributes where rowid = ?1 and type = ?3) \
             and not exists (select 1 from Attributes where parent = ?1 and name = ?2)",
        )?
        .query_row(params![parent, name, ATTR_FULL_ATTRS], |row| row.get(0))?;
    Ok(unlisted)
}

fn seal(tx: &Transaction<'_>, parent: i64, name: &str, children: &[String]) -> Result<()> {
    let rowid: i64 = tx
        .prepare_cached(
            "insert into Attributes (parent, name, type, value) values (?1, ?2, ?3, null) \
             on conflict (parent, name) do update set type = excluded.type, value = excluded.value \
             returning rowid",
        )?
        .query_row(params![parent, name, ATTR_FULL_ATTRS], |row| row.get(0))?;
    tx.prepare_cached("delete from Attributes where parent = ?1 and type = ?2")?
        .execute(params![rowid, ATTR_MISSING])?;
    for child in children {
        tx.prepare_cached(
            "insert or ignore into Attributes (parent, name, type) values (?1, ?2, ?3)",
        )?
        .execute(params![rowid, child, ATTR_PLACEHOLDER])?;
    }

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
    use gradient_storage::eval_cache_segment::decode;
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

    fn nix_seal(conn: &Connection, parent: i64, name: &str, children: &[&str]) -> i64 {
        let rowid = conn
            .query_row(
                "insert into Attributes(parent, name, type, value) values (?, ?, ?, ?) \
                 on conflict(parent, name) do update set type = excluded.type, value = excluded.value \
                 returning rowid",
                params![parent, name, ATTR_FULL_ATTRS, None::<String>],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "delete from Attributes where parent = ? and type = 3",
            [rowid],
        )
        .unwrap();
        for child in children {
            conn.execute(
                "insert or ignore into Attributes(parent, name, type, value) values (?, ?, ?, ?)",
                params![rowid, child, ATTR_PLACEHOLDER, None::<String>],
            )
            .unwrap();
        }

        rowid
    }

    fn listing(names: &[&str]) -> String {
        child_listing(names.iter().map(|name| (*name).to_owned()).collect())
    }

    fn children(conn: &Connection, parent: i64) -> Vec<String> {
        let mut statement = conn
            .prepare("select name from Attributes where parent = ?1 order by name")
            .unwrap();
        statement
            .query_map([parent], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    }

    fn root_rowid(conn: &Connection) -> i64 {
        conn.query_row(
            "select rowid from Attributes where parent = 0 and name = ''",
            [],
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
            .flat_map(|(_, segment)| decode(segment).unwrap())
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
        let root = nix_seal(&nix, 0, "", &[]);
        nix_insert(&nix, root, "hello", ATTR_STRING, Some("world"));

        let rows = exported_rows(&mut file);

        assert_eq!(
            rows,
            vec![
                row(&[], ATTR_FULL_ATTRS, Some(&listing(&["hello"]))),
                row(&["hello"], ATTR_STRING, Some("world")),
            ]
        );
        assert!(file.export_changes().unwrap().is_empty());
    }

    #[test]
    fn a_nix_upsert_of_a_row_changed_since_the_last_export_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(&dir);
        let mut file = open(&path).unwrap();
        let nix = nix(&path);
        let root = nix_insert(&nix, 0, "", ATTR_PLACEHOLDER, None);
        nix_seal(&nix, root, "hydraJobs", &[]);

        nix_seal(&nix, root, "hydraJobs", &["perl"]);

        assert_eq!(children(&nix, root), vec!["hydraJobs"]);
        assert_eq!(
            lookup(&nix, &["hydraJobs", "perl"]),
            Some((ATTR_PLACEHOLDER, None))
        );
        assert_eq!(exported_rows(&mut file).len(), 3);
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
        let root = nix_seal(&nix, 0, "", &[]);
        let a = nix_insert(&nix, root, "a", ATTR_PLACEHOLDER, None);
        nix_insert(&nix, a, "b", ATTR_STRING, Some("x"));
        nix_insert(&nix, root, "a", ATTR_STRING, Some("y"));

        let rows = exported_rows(&mut file);

        assert_eq!(
            rows,
            vec![
                row(&[], ATTR_FULL_ATTRS, Some(&listing(&["a"]))),
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
    fn export_carries_the_child_listing_of_a_full_attrs_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(&dir);
        let mut file = open(&path).unwrap();
        let nix = nix(&path);
        let root = nix_seal(&nix, 0, "", &["b\tc", "a"]);
        nix_insert(&nix, root, "type", ATTR_MISSING, None);

        let rows = exported_rows(&mut file);

        assert_eq!(rows[0].path, Vec::<String>::new());
        assert_eq!(
            listed_children(rows[0].value.as_deref()).unwrap(),
            vec!["a".to_owned(), "b\tc".to_owned()]
        );
    }

    #[test]
    fn a_full_attrs_row_alone_yields_its_children_as_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(&dir);
        let mut file = open(&path).unwrap();
        let nix = nix(&path);
        let root = nix_insert(&nix, 0, "", ATTR_PLACEHOLDER, None);
        nix_insert(&nix, root, "a", ATTR_STRING, Some("x"));
        nix_insert(&nix, root, "type", ATTR_MISSING, None);
        file.export_changes().unwrap();

        file.apply(
            "k",
            &[row(&[], ATTR_FULL_ATTRS, Some(&listing(&["a", "b"])))],
        )
        .unwrap();

        assert_eq!(lookup(&nix, &[]), Some((ATTR_FULL_ATTRS, None)));
        assert_eq!(root_rowid(&nix), root);
        assert_eq!(children(&nix, root), vec!["a", "b"]);
        assert_eq!(
            lookup(&nix, &["a"]),
            Some((ATTR_STRING, Some("x".to_owned())))
        );
        assert_eq!(lookup(&nix, &["b"]), Some((ATTR_PLACEHOLDER, None)));
        assert!(file.export_changes().unwrap().is_empty());
    }

    #[test]
    fn a_missing_row_exported_before_the_seal_leaves_no_phantom_child() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(&dir);
        let mut source = open(&path).unwrap();
        let writer = nix(&path);
        let root = nix_insert(&writer, 0, "", ATTR_PLACEHOLDER, None);
        nix_insert(&writer, root, "type", ATTR_MISSING, None);
        let before_seal = source.export_changes().unwrap();
        nix_seal(&writer, 0, "", &["a"]);
        let after_seal = source.export_changes().unwrap();
        let rows_of = |segments: &[(i64, Vec<u8>)]| -> Vec<EvalCacheRow> {
            segments
                .iter()
                .flat_map(|(_, segment)| decode(segment).unwrap())
                .collect()
        };
        let (early, late) = (rows_of(&before_seal), rows_of(&after_seal));

        for order in [[&early, &late], [&late, &early]] {
            let other = tempfile::tempdir().unwrap();
            let other_path = cache_path(&other);
            let mut receiver = open(&other_path).unwrap();
            for (index, rows) in order.into_iter().enumerate() {
                receiver.apply(&format!("k{index}"), rows).unwrap();
            }

            let receiver_nix = nix(&other_path);
            assert_eq!(
                children(&receiver_nix, root_rowid(&receiver_nix)),
                vec!["a"]
            );
        }
    }
}
