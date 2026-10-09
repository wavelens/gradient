/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The settings are an explicit key list, not a serialization of the config. This part is likely to
//! gain a secret field later. Adding a key must be a deliberate edit here.

use anyhow::{Context as _, Result};
use gradient_types::RuntimeConfig;
use rusqlite::Connection;

pub fn write_config_snapshot(conn: &Connection, config: &RuntimeConfig) -> Result<()> {
    conn.execute(
        "CREATE TABLE config_snapshot (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .context("create config_snapshot")?;

    let mut entries: Vec<(&str, String)> = vec![
        (
            "inputs_unavailable_max_loops",
            config.build.inputs_unavailable_max_loops.to_string(),
        ),
        ("build_max_attempts", config.build.max_attempts.to_string()),
        (
            "worker_heartbeat_timeout_secs",
            config.proto.worker_heartbeat_timeout_secs.to_string(),
        ),
        (
            "nar_storage_open_timeout_secs",
            config.nar.storage_open_timeout_secs.to_string(),
        ),
        (
            "nar_send_chunk_timeout_secs",
            config.nar.send_chunk_timeout_secs.to_string(),
        ),
        ("nar_chunk_bytes", config.nar.chunk_bytes.to_string()),
        (
            "max_concurrent_nar_serves",
            config.nar.max_concurrent_serves.to_string(),
        ),
        (
            "upstream_query_concurrency",
            config.cache.upstream_query_concurrency.to_string(),
        ),
        ("upload_concurrency", config.upload.concurrency.to_string()),
        (
            "upload_bytes_budget",
            config.upload.bytes_budget.to_string(),
        ),
        (
            "upload_lease_idle_secs",
            config.upload.lease_idle_secs.to_string(),
        ),
        (
            "upload_rest_wait_secs",
            config.upload.rest_wait_secs.to_string(),
        ),
        ("nar_ttl_hours", config.gc.nar_ttl_hours.to_string()),
        (
            "nar_upload_grace_hours",
            config.gc.nar_upload_grace_hours.to_string(),
        ),
        ("nar_verify_digest", config.nar.verify_digest.to_string()),
    ];

    // The resolved S3 policy is written instead of the raw arguments. A local-disk instance is
    // saying so instead.
    match &config.s3 {
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

    fn config() -> RuntimeConfig {
        RuntimeConfig::from_cli(&gradient_types::Cli::default()).expect("default config")
    }

    #[test]
    fn config_snapshot_is_an_explicit_key_list_with_no_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        write_config_snapshot(&conn, &config()).expect("snapshot");

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

    #[test]
    fn the_self_heal_threshold_is_present_and_real() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        let config = config();
        write_config_snapshot(&conn, &config).expect("snapshot");

        let value: String = conn
            .query_row(
                "SELECT value FROM config_snapshot WHERE key = 'inputs_unavailable_max_loops'",
                [],
                |r| r.get(0),
            )
            .expect("value");
        assert_eq!(value, config.build.inputs_unavailable_max_loops.to_string());
    }
}
