/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Content-integrity verification for stored NAR bytes: recompute the narinfo
//! `file_hash` (SHA-256 SRI) over the object and compare it against the value a
//! worker or client reported at upload time.

use gradient_util::nix_hash::normalize_nar_hash;
use harmonia_utils_hash::{Algorithm, Context, HashFormat as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt as _};

/// SHA-256 of `bytes` as a narinfo `file_hash` SRI string (`sha256-<base64>`),
/// matching how uploads compute the value they report.
pub fn file_hash_sri(bytes: &[u8]) -> String {
    Sha256::digest(bytes).as_sri().to_string()
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

/// Verify `bytes` against the reported `file_hash` and `size`. The size check is
/// unconditional; the content-hash check runs only for SHA-256 expected hashes,
/// so a legacy non-sha256 (e.g. blake3) upload is size-verified rather than
/// falsely rejected.
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

/// Streaming counterpart to [`verify_nar_bytes`]: reads `reader` to end while
/// hashing incrementally, so a large NAR is never held whole in memory. The
/// incremental SHA-256 uses harmonia's `Context`, which is documented to match
/// the one-shot `Sha256::digest` used by [`file_hash_sri`] exactly, so the
/// computed `file_hash` SRI is identical to the buffered path.
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

    #[test]
    fn valid_bytes_pass() {
        let expected = file_hash_sri(BYTES);
        assert!(verify_nar_bytes(BYTES, &expected, BYTES.len() as u64).is_ok());
    }

    #[test]
    fn size_mismatch_is_size_error() {
        let expected = file_hash_sri(BYTES);
        let err = verify_nar_bytes(BYTES, &expected, BYTES.len() as u64 + 1).unwrap_err();
        assert!(matches!(err, VerifyError::Size { .. }));
    }

    #[test]
    fn tampered_bytes_are_hash_error() {
        let mut tampered = BYTES.to_vec();
        let expected = file_hash_sri(&tampered);
        *tampered.last_mut().unwrap() ^= 0xff;
        let err = verify_nar_bytes(&tampered, &expected, tampered.len() as u64).unwrap_err();
        assert!(matches!(err, VerifyError::Hash { .. }));
    }

    #[test]
    fn non_sha256_hash_takes_size_only_path() {
        let blake3 = "blake3:11cxppanr71mzl1xnyax8rccaj5milx2fx9vnvzk6la672nb6dv4";
        assert!(verify_nar_bytes(BYTES, blake3, BYTES.len() as u64).is_ok());
    }

    /// The streaming verifier's incremental SHA-256 must produce the exact
    /// `file_hash` SRI the one-shot path reports, so a valid NAR passes.
    #[tokio::test]
    async fn reader_verify_matches_one_shot_hash() {
        let expected = file_hash_sri(BYTES);
        assert!(
            verify_nar_reader(BYTES, &expected, BYTES.len() as u64)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn reader_verify_rejects_size_mismatch() {
        let expected = file_hash_sri(BYTES);
        let err = verify_nar_reader(BYTES, &expected, BYTES.len() as u64 + 1)
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Size { .. }));
    }

    #[tokio::test]
    async fn reader_verify_rejects_tampered_content() {
        let mut tampered = BYTES.to_vec();
        let expected = file_hash_sri(&tampered);
        *tampered.last_mut().unwrap() ^= 0xff;
        let err = verify_nar_reader(&tampered[..], &expected, tampered.len() as u64)
            .await
            .unwrap_err();
        assert!(matches!(err, VerifyError::Hash { .. }));
    }
}
