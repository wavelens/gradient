/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::sources::SourceError;
use tracing::debug;

const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// Reads pkt-lines from `reader` and returns the SHA-1 hash for the wanted ref.
///
/// `target = None` → return `HEAD`, falling back to the first non-zero ref
/// (matches libgit2's `list.first()` behaviour for repos advertising only
/// `capabilities^{}`).
/// `target = Some("refs/heads/main")` → return that exact ref with no fallback;
/// `GitHashExtraction` if it is not advertised.
///
/// Reads incrementally - one pkt-line at a time - so it works correctly even
/// when the remote keeps the connection open after the ref advertisement
/// (which is normal git protocol behavior).
pub(super) fn read_ref_from_pktlines(
    reader: &mut dyn std::io::Read,
    target: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    let want = target.unwrap_or("HEAD");
    let allow_fallback = target.is_none();
    let mut len_buf = [0u8; 4];
    let mut first_ref: Option<Vec<u8>> = None;

    loop {
        std::io::Read::read_exact(reader, &mut len_buf).map_err(|e| {
            SourceError::GitCommandFailed {
                stderr: e.to_string(),
            }
        })?;

        let len = std::str::from_utf8(&len_buf)
            .ok()
            .and_then(|s| usize::from_str_radix(s, 16).ok())
            .ok_or(SourceError::GitOutputParsing)?;

        if len == 0 {
            break; // flush pkt - end of advertisement
        }

        if len < 4 {
            break;
        }

        let payload_len = len - 4;
        let mut data = vec![0u8; payload_len];
        std::io::Read::read_exact(reader, &mut data).map_err(|e| {
            SourceError::GitCommandFailed {
                stderr: e.to_string(),
            }
        })?;

        // Ref lines: "<40-hex-sha1> <refname>[NUL capabilities]\n"
        if data.len() >= 41 && data[40] == b' ' {
            let sha = match std::str::from_utf8(&data[..40]) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let ref_bytes = &data[41..];
            let refname_end = ref_bytes
                .iter()
                .position(|&b| b == 0 || b == b'\n')
                .unwrap_or(ref_bytes.len());

            let refname = std::str::from_utf8(&ref_bytes[..refname_end])
                .unwrap_or("")
                .trim();

            debug!(refname, sha, "pkt-line ref");

            if refname == want {
                return hex::decode(sha).map_err(|_| SourceError::GitOutputParsing);
            }

            // Remember the first real ref as HEAD fallback (skip the zero-id
            // capabilities marker); only relevant when polling HEAD.
            if allow_fallback
                && first_ref.is_none()
                && sha != ZERO_SHA
                && let Ok(bytes) = hex::decode(sha)
            {
                first_ref = Some(bytes);
            }
        } else {
            // Non-ref pkt-line (e.g. version advertisement).
            let preview = std::str::from_utf8(&data).unwrap_or("<binary>").trim_end();
            debug!(preview, "pkt-line non-ref");
        }
    }

    // HEAD path falls back to the first non-zero ref; the exact-branch path
    // left `first_ref` untouched, so this is `GitHashExtraction` for it.
    first_ref.ok_or(SourceError::GitHashExtraction)
}
