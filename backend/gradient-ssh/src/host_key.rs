/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey};
use std::path::Path;
use tokio::io::AsyncWriteExt as _;

const GENERATED_KEY: &str = "ssh_host_ed25519_key";

pub async fn load_or_generate(file: Option<&str>, base_dir: &Path) -> anyhow::Result<PrivateKey> {
    if let Some(file) = file {
        return Ok(russh::keys::load_secret_key(file, None)?);
    }

    let path = base_dir.join(GENERATED_KEY);
    if tokio::fs::try_exists(&path).await? {
        return Ok(russh::keys::load_secret_key(&path, None)?);
    }

    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let pem = key.to_openssh(LineEnding::LF)?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .await?;
    file.write_all(pem.as_bytes()).await?;
    file.sync_all().await?;

    tracing::info!(path = %path.display(), "generated ssh host key");
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_generated_host_key_is_reused() {
        let dir = tempfile::tempdir().expect("tmp");
        let first = load_or_generate(None, dir.path()).await.expect("key");
        let second = load_or_generate(None, dir.path()).await.expect("key");
        assert_eq!(first.public_key(), second.public_key());
    }

    #[tokio::test]
    async fn a_configured_host_key_is_loaded() {
        let dir = tempfile::tempdir().expect("tmp");
        let generated = load_or_generate(None, dir.path()).await.expect("key");
        let file = dir.path().join("ssh_host_ed25519_key");

        let loaded = load_or_generate(file.to_str(), Path::new("/nonexistent"))
            .await
            .expect("key");
        assert_eq!(generated.public_key(), loaded.public_key());
    }
}
