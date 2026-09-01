/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The resolved settings that decide how a symptom should be read.
//!
//! An explicit key list rather than a serialization of the config: this is the
//! part most likely to gain a secret field later and least likely to be
//! re-reviewed, so adding one has to be a deliberate edit here.

use anyhow::{Context as _, Result};
use gradient_types::{EvalArgs, ProtoArgs, S3Config, StorageArgs};
use rusqlite::Connection;

/// Takes the four argument groups it actually reads rather than the whole
/// `Cli`, so the dependency surface is honest and a test can build one.
pub fn write_config_snapshot(
    conn: &Connection,
    eval: &EvalArgs,
    proto: &ProtoArgs,
    storage: &StorageArgs,
    s3: Option<&S3Config>,
) -> Result<()> {
    conn.execute(
        "CREATE TABLE config_snapshot (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .context("create config_snapshot")?;

    let mut entries: Vec<(&str, String)> = vec![
        (
            "inputs_unavailable_max_loops",
            eval.inputs_unavailable_max_loops.to_string(),
        ),
        ("build_max_attempts", eval.build_max_attempts.to_string()),
        (
            "worker_heartbeat_timeout_secs",
            proto.worker_heartbeat_timeout_secs.to_string(),
        ),
        (
            "nar_storage_open_timeout_secs",
            proto.nar_storage_open_timeout_secs.to_string(),
        ),
        (
            "nar_send_chunk_timeout_secs",
            proto.nar_send_chunk_timeout_secs.to_string(),
        ),
        (
            "max_concurrent_nar_serves",
            proto.max_concurrent_nar_serves.to_string(),
        ),
        (
            "upstream_query_concurrency",
            proto.upstream_query_concurrency.to_string(),
        ),
        ("nar_ttl_hours", storage.nar_ttl_hours.to_string()),
        (
            "nar_upload_grace_hours",
            storage.nar_upload_grace_hours.to_string(),
        ),
        ("nar_verify_digest", storage.nar_verify_digest.to_string()),
    ];

    // The resolved S3 policy, not the raw arguments, so the file says what the
    // server is actually doing; a local-disk instance says so instead.
    match s3 {
        Some(s3) => {
            entries.push(("storage_backend", "s3".to_owned()));
            entries.push((
                "s3_read_timeout_secs",
                s3.read_timeout.as_secs().to_string(),
            ));
            entries.push(("s3_max_retries", s3.max_retries.to_string()));
            entries.push((
                "s3_retry_timeout_secs",
                s3.retry_timeout.as_secs().to_string(),
            ));
        }
        None => entries.push(("storage_backend", "local disk".to_owned())),
    }

    for (key, value) in entries {
        conn.execute(
            "INSERT INTO config_snapshot VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .context("write config_snapshot")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::open_report;

    #[test]
    fn config_snapshot_is_an_explicit_key_list_with_no_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        write_config_snapshot(
            &conn,
            &EvalArgs::default(),
            &ProtoArgs::default(),
            &StorageArgs::default(),
            None,
        )
        .expect("snapshot");

        let keys: Vec<String> = conn
            .prepare("SELECT key FROM config_snapshot")
            .and_then(|mut s| s.query_map([], |r| r.get(0)).and_then(|m| m.collect()))
            .expect("keys");

        assert!(keys.contains(&"inputs_unavailable_max_loops".to_string()));
        assert!(keys.contains(&"worker_heartbeat_timeout_secs".to_string()));
        for key in &keys {
            assert!(
                !key.contains("secret") && !key.contains("password") && !key.contains("token"),
                "config snapshot leaked {key}"
            );
        }
    }

    /// The circuit breaker threshold is the number a self-heal loop has to be
    /// read against, so it must survive into the report.
    #[test]
    fn the_self_heal_threshold_is_present_and_real() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        let eval = EvalArgs::default();
        write_config_snapshot(
            &conn,
            &eval,
            &ProtoArgs::default(),
            &StorageArgs::default(),
            None,
        )
        .expect("snapshot");

        let value: String = conn
            .query_row(
                "SELECT value FROM config_snapshot WHERE key = 'inputs_unavailable_max_loops'",
                [],
                |r| r.get(0),
            )
            .expect("value");
        assert_eq!(value, eval.inputs_unavailable_max_loops.to_string());
    }
}
