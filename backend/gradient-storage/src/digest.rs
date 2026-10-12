/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_util::nix_hash::normalize_nar_hash;
use harmonia_utils_hash::{Algorithm, Context, HashFormat as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt as _};

pub fn file_hash_sri(bytes: &[u8]) -> String {
    Sha256::digest(bytes).as_sri().to_string()
}

pub fn file_hash_matches(expected: &str, actual: &[u8; 32]) -> bool {
    let expected_norm = normalize_nar_hash(expected);
    if !expected_norm.starts_with("sha256:") {
        return true;
    }

    normalize_nar_hash(&Sha256::new(actual).as_sri().to_string()) == expected_norm
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("NAR object missing from storage")]
    Missing,
    #[error("NAR size mismatch: expected {expected} bytes, got {actual}")]
    Size { expected: u64, actual: u64 },
    #[error("NAR content hash mismatch: expected {expected}, computed {computed}")]
    Hash { expected: String, computed: String },
    #[error(transparent)]
    Store(#[from] anyhow::Error),
}

/// The size check is unconditional. A legacy non-sha256 upload such as blake3 is only size-verified
/// instead of being falsely rejected.
pub fn verify_nar_bytes(
    bytes: &[u8],
    expected_file_hash: &str,
    expected_size: u64,
) -> Result<(), VerifyError> {
    let actual = bytes.len() as u64;
    if actual != expected_size {
        return Err(VerifyError::Size {
            expected: expected_size,
            actual,
        });
    }

    let expected_norm = normalize_nar_hash(expected_file_hash);
    if expected_norm.starts_with("sha256:") {
        let computed_norm = normalize_nar_hash(&file_hash_sri(bytes));
        if computed_norm != expected_norm {
            return Err(VerifyError::Hash {
                expected: expected_norm,
                computed: computed_norm,
            });
        }
    }

    Ok(())
}

pub async fn verify_nar_reader<R: AsyncRead + Unpin>(
    mut reader: R,
    expected_file_hash: &str,
    expected_size: u64,
) -> Result<(), VerifyError> {
    let expected_norm = normalize_nar_hash(expected_file_hash);
    let mut ctx = expected_norm
        .starts_with("sha256:")
        .then(|| Context::new(Algorithm::SHA256));

    let mut total: u64 = 0;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|e| VerifyError::Store(e.into()))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if let Some(ctx) = ctx.as_mut() {
            ctx.update(&buf[..n]);
        }
    }

    if total != expected_size {
        return Err(VerifyError::Size {
            expected: expected_size,
            actual: total,
        });
    }

    if let Some(ctx) = ctx {
        let computed = Sha256::try_from(ctx.finish()).map_err(|_| {
            VerifyError::Store(anyhow::anyhow!("sha256 finalize produced non-sha256"))
        })?;
        let computed_norm = normalize_nar_hash(&computed.as_sri().to_string());
        if computed_norm != expected_norm {
            return Err(VerifyError::Hash {
                expected: expected_norm,
                computed: computed_norm,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BYTES: &[u8] = b"gradient nar integrity check";

    #[tokio::test]
    async fn valid_bytes_pass_one_shot_and_streaming() {
        let expected = file_hash_sri(BYTES);
        let size = BYTES.len() as u64;
        assert!(verify_nar_bytes(BYTES, &expected, size).is_ok());
        assert!(verify_nar_reader(BYTES, &expected, size).await.is_ok());
    }

    #[tokio::test]
    async fn a_size_mismatch_is_a_size_error_one_shot_and_streaming() {
        let expected = file_hash_sri(BYTES);
        let size = BYTES.len() as u64 + 1;
        assert!(matches!(
            verify_nar_bytes(BYTES, &expected, size).unwrap_err(),
            VerifyError::Size { .. }
        ));
        assert!(matches!(
            verify_nar_reader(BYTES, &expected, size).await.unwrap_err(),
            VerifyError::Size { .. }
        ));
    }

    #[tokio::test]
    async fn tampered_bytes_are_a_hash_error_one_shot_and_streaming() {
        let mut tampered = BYTES.to_vec();
        let expected = file_hash_sri(&tampered);
        *tampered.last_mut().unwrap() ^= 0xff;
        let size = tampered.len() as u64;
        assert!(matches!(
            verify_nar_bytes(&tampered, &expected, size).unwrap_err(),
            VerifyError::Hash { .. }
        ));
        assert!(matches!(
            verify_nar_reader(&tampered[..], &expected, size)
                .await
                .unwrap_err(),
            VerifyError::Hash { .. }
        ));
    }

    #[test]
    fn file_hash_matches_compares_sha256_only() {
        let digest = *Sha256::digest(BYTES).digest_bytes();
        assert!(file_hash_matches(&file_hash_sri(BYTES), &digest));
        assert!(file_hash_matches(
            &normalize_nar_hash(&file_hash_sri(BYTES)),
            &digest
        ));

        let mut other = digest;
        other[0] ^= 0xff;
        assert!(!file_hash_matches(&file_hash_sri(BYTES), &other));

        let blake3 = "blake3:11cxppanr71mzl1xnyax8rccaj5milx2fx9vnvzk6la672nb6dv4";
        assert!(file_hash_matches(blake3, &[0u8; 32]));
    }

    #[test]
    fn non_sha256_hash_takes_size_only_path() {
        let blake3 = "blake3:11cxppanr71mzl1xnyax8rccaj5milx2fx9vnvzk6la672nb6dv4";
        assert!(verify_nar_bytes(BYTES, blake3, BYTES.len() as u64).is_ok());
    }
}
