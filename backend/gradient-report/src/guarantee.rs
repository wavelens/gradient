/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The proof that anonymisation holds.
//!
//! The claim is a property of the produced *file*, so these build a real report
//! from rows they control and read the result back. No database, and nothing
//! that breaks when extraction order changes.

#[cfg(test)]
mod tests {
    use crate::extract::{create_table, redact_row, write_rows};
    use crate::logs::{create_log_table, insert_log};
    use crate::redact::Redactor;
    use crate::schema::{ReportOptions, open_report};
    use crate::tables::{Row, TableSpec, eval_scope_tables};

    const REPO: &str = "git@git.supersandro.de:sandro/nixos-config.git";
    const EMAIL: &str = "sandro@example.invalid";
    const PKG: &str = "clap_complete-4.6.9";
    const STORE_PATH: &str = "/nix/store/2s7ijz3qblblfb903r4spy3pvd7ag35f-clap_complete-4.6.9";

    fn spec(name: &str) -> &'static TableSpec {
        eval_scope_tables()
            .iter()
            .find(|s| s.name == name)
            .expect("spec exists")
    }

    fn row_with(spec: &TableSpec, values: &[(&str, &str)]) -> Row {
        let mut row = vec![None; spec.columns.len()];
        for (column, value) in values {
            let index = spec
                .columns
                .iter()
                .position(|c| c == column)
                .unwrap_or_else(|| panic!("{} has no column {column}", spec.name));
            row[index] = Some((*value).to_owned());
        }
        row
    }

    /// Build a report carrying the seeded strings in every column the policy
    /// claims to cover, plus a log that embeds them in free text.
    fn write_seeded_report(conn: &rusqlite::Connection, redactor: &Redactor) {
        let seeds: [(&str, Vec<(&str, &str)>); 4] = [
            (
                "evaluation",
                vec![
                    ("id", "01a05a38-3276-7252-bc05-c139d9c8a015"),
                    ("repository", REPO),
                    ("flake_source", REPO),
                    ("started_by", EMAIL),
                    ("status", "7"),
                ],
            ),
            (
                "derivation",
                vec![("name", PKG), ("pname", "clap_complete")],
            ),
            (
                "derivation_output",
                vec![
                    ("package", PKG),
                    ("deriver", STORE_PATH),
                    ("references_list", STORE_PATH),
                ],
            ),
            (
                "cached_path",
                vec![("package", PKG), ("deriver", STORE_PATH)],
            ),
        ];

        for (name, values) in seeds {
            let spec = spec(name);
            create_table(conn, spec).expect("ddl");
            let row = row_with(spec, &values);
            write_rows(conn, spec, &[redact_row(spec, redactor, &row)]).expect("write");
        }

        create_log_table(conn).expect("log ddl");
        insert_log(
            conn,
            redactor,
            "0199-attempt",
            &format!("error: while building {STORE_PATH} for {REPO}"),
        )
        .expect("log");
    }

    /// Walk every column of every table and count hits. `SELECT *` is correct
    /// here and only here: the test must read whatever the file happens to
    /// contain, which is the opposite of the extractor's rule.
    fn count_occurrences(conn: &rusqlite::Connection, needle: &str) -> usize {
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .and_then(|mut s| s.query_map([], |r| r.get(0)).and_then(|m| m.collect()))
            .expect("tables");

        let mut hits = 0;
        for table in tables {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM \"{table}\""))
                .expect("select");
            let columns = stmt.column_count();
            let mut rows = stmt.query([]).expect("query");
            while let Some(row) = rows.next().expect("row") {
                for i in 0..columns {
                    if let Ok(text) = row.get::<_, String>(i)
                        && text.contains(needle)
                    {
                        hits += 1;
                    }
                }
            }
        }

        hits
    }

    fn opts(identities: bool, packages: bool) -> ReportOptions {
        ReportOptions {
            anonymize_identities: identities,
            anonymize_packages: packages,
            include_logs: true,
            include_instance: true,
        }
    }

    /// A redaction feature without this test is a promise, not a guarantee.
    #[test]
    fn an_anonymized_report_contains_no_original_identifier() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r.db");
        let conn = open_report(&path).expect("open");
        write_seeded_report(&conn, &Redactor::new(opts(true, true)));
        drop(conn);

        let conn = rusqlite::Connection::open(&path).expect("reopen");
        for needle in [
            REPO,
            EMAIL,
            PKG,
            "supersandro",
            "nixos-config",
            "clap_complete",
        ] {
            assert_eq!(
                count_occurrences(&conn, needle),
                0,
                "anonymized report still contains {needle}"
            );
        }
    }

    /// The hash half is one-way and is what makes an upstream cache check
    /// possible, so full anonymisation must not take it with the name.
    #[test]
    fn store_path_hashes_survive_full_anonymization() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r.db");
        let conn = open_report(&path).expect("open");
        write_seeded_report(&conn, &Redactor::new(opts(true, true)));
        drop(conn);

        let conn = rusqlite::Connection::open(&path).expect("reopen");
        assert!(
            count_occurrences(&conn, "2s7ijz3qblblfb903r4spy3pvd7ag35f") > 0,
            "the store hash must survive: it is what a cache check needs"
        );
    }

    /// The complement, so the test above is proving redaction rather than
    /// proving data loss.
    #[test]
    fn an_unanonymized_report_keeps_every_identifier() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r.db");
        let conn = open_report(&path).expect("open");
        write_seeded_report(&conn, &Redactor::new(opts(false, false)));
        drop(conn);

        let conn = rusqlite::Connection::open(&path).expect("reopen");
        assert!(count_occurrences(&conn, REPO) > 0);
        assert!(count_occurrences(&conn, PKG) > 0);
        assert!(count_occurrences(&conn, EMAIL) > 0);
    }

    /// The toggles are independent, so a report may name packages while hiding
    /// who owns them. That combination is the shipped default.
    #[test]
    fn packages_can_be_kept_while_identities_are_hidden() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r.db");
        let conn = open_report(&path).expect("open");
        write_seeded_report(&conn, &Redactor::new(opts(true, false)));
        drop(conn);

        let conn = rusqlite::Connection::open(&path).expect("reopen");
        assert_eq!(count_occurrences(&conn, REPO), 0);
        assert_eq!(count_occurrences(&conn, EMAIL), 0);
        assert!(
            count_occurrences(&conn, PKG) > 0,
            "knowing which package broke is the point of the separate toggle"
        );
    }
}
