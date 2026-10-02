/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::SourceError;
use tracing::debug;

const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// Pkt-lines are read one at a time. The remote is normally keeping the connection open after the
/// ref advertisement.
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
            break;
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

            if allow_fallback
                && first_ref.is_none()
                && sha != ZERO_SHA
                && let Ok(bytes) = hex::decode(sha)
            {
                first_ref = Some(bytes);
            }
        } else {
            let preview = std::str::from_utf8(&data).unwrap_or("<binary>").trim_end();
            debug!(preview, "pkt-line non-ref");
        }
    }

    first_ref.ok_or(SourceError::GitHashExtraction)
}
