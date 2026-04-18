/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Persistent worker-ID management.
//!
//! The worker ID is a UUID stored in `<data_dir>/worker-id`.  If the file
//! does not exist it is created with a freshly generated UUID.  An explicit
//! override can be supplied via the `GRADIENT_WORKER_ID` environment variable
//! (already parsed into `config.worker_id`).

use anyhow::{Context, Result};
use tracing::info;

/// Load the worker ID from `<data_dir>/worker-id`, or generate a new UUID
/// and persist it.  If `id_override` is `Some`, it is used directly
/// (must be a valid UUID string).
pub(super) fn load_or_generate_id(data_dir: &str, id_override: Option<&str>) -> Result<String> {
    use std::fs;
    use std::path::Path;

    if let Some(raw) = id_override {
        let id = raw.trim().to_owned();
        id.parse::<uuid::Uuid>()
            .with_context(|| format!("GRADIENT_WORKER_ID is not a valid UUID: {:?}", id))?;
        info!(%id, "using worker ID from GRADIENT_WORKER_ID");
        return Ok(id);
    }

    let dir = Path::new(data_dir);
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create data directory '{}'", data_dir))?;

    let id_path = dir.join("worker-id");

    if id_path.exists() {
        let raw = fs::read_to_string(&id_path)
            .with_context(|| format!("failed to read '{}'", id_path.display()))?;
        let id = raw.trim().to_owned();
        id.parse::<uuid::Uuid>().with_context(|| {
            format!("'{}' contains an invalid UUID: {:?}", id_path.display(), id)
        })?;
        info!(path = %id_path.display(), %id, "loaded persistent worker ID");
        return Ok(id);
    }

    let id = uuid::Uuid::new_v4().to_string();
    fs::write(&id_path, &id).with_context(|| format!("failed to write '{}'", id_path.display()))?;
    info!(path = %id_path.display(), %id, "generated and persisted new worker ID");
    Ok(id)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    #[test]
    fn load_or_generate_id_creates_new() {
        let dir = temp_dir();
        let data_dir = dir.path().to_string_lossy().to_string();
        let id = load_or_generate_id(&data_dir, None).expect("should generate id");
        id.parse::<uuid::Uuid>()
            .expect("generated id must be a valid UUID");
        let id_path = dir.path().join("worker-id");
        assert!(id_path.exists(), "worker-id file should be created");
        assert_eq!(fs::read_to_string(&id_path).unwrap().trim(), id);
    }

    #[test]
    fn load_or_generate_id_reads_existing() {
        let dir = temp_dir();
        let id_path = dir.path().join("worker-id");
        let known_id = uuid::Uuid::new_v4().to_string();
        fs::write(&id_path, &known_id).unwrap();
        let data_dir = dir.path().to_string_lossy().to_string();
        let loaded = load_or_generate_id(&data_dir, None).expect("should read existing id");
        assert_eq!(loaded, known_id);
    }

    #[test]
    fn load_or_generate_id_invalid_uuid_fails() {
        let dir = temp_dir();
        let id_path = dir.path().join("worker-id");
        fs::write(&id_path, "not-a-uuid").unwrap();
        let data_dir = dir.path().to_string_lossy().to_string();
        let result = load_or_generate_id(&data_dir, None);
        assert!(result.is_err(), "invalid UUID in file should return Err");
    }

    #[test]
    fn load_or_generate_id_override_takes_priority() {
        let dir = temp_dir();
        let id_path = dir.path().join("worker-id");
        let file_id = uuid::Uuid::new_v4().to_string();
        fs::write(&id_path, &file_id).unwrap();
        let override_id = uuid::Uuid::new_v4().to_string();
        let data_dir = dir.path().to_string_lossy().to_string();
        let result =
            load_or_generate_id(&data_dir, Some(&override_id)).expect("override should work");
        assert_eq!(result, override_id, "override must win over file");
    }

    #[test]
    fn load_or_generate_id_override_invalid_uuid_fails() {
        let dir = temp_dir();
        let data_dir = dir.path().to_string_lossy().to_string();
        let result = load_or_generate_id(&data_dir, Some("not-a-uuid"));
        assert!(result.is_err(), "invalid override UUID should return Err");
    }
}
