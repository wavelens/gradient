/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::remote::AdvertisedRef;
use crate::SourceError;
use tracing::debug;

/// Stop at the flush packet. The remote is normally keeping the connection open after the ref
/// advertisement.
pub(super) fn read_advertised_refs(
    reader: &mut dyn std::io::Read,
) -> Result<Vec<AdvertisedRef>, SourceError> {
    let mut refs = Vec::new();
    let mut head_target = None;

    while let Some(line) = read_pkt_line(reader)? {
        let Some((oid, name, capabilities)) = parse_ref_line(&line) else {
            let preview = std::str::from_utf8(&line).unwrap_or("<binary>").trim_end();
            debug!(preview, "pkt-line non-ref");
            continue;
        };
        debug!(refname = name, "pkt-line ref");
        head_target = head_target.or_else(|| head_symref(capabilities));
        refs.push(AdvertisedRef {
            name: name.to_owned(),
            oid,
            symref_target: None,
        });
    }

    if let Some(head) = refs.iter_mut().find(|r| r.name == "HEAD") {
        head.symref_target = head_target;
    }
    Ok(refs)
}

fn read_pkt_line(reader: &mut dyn std::io::Read) -> Result<Option<Vec<u8>>, SourceError> {
    let read_failed = |e: std::io::Error| SourceError::GitCommandFailed {
        stderr: e.to_string(),
    };
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).map_err(read_failed)?;

    let len = std::str::from_utf8(&len_buf)
        .ok()
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .ok_or(SourceError::GitOutputParsing)?;
    if len < 4 {
        return Ok(None);
    }

    let mut data = vec![0u8; len - 4];
    reader.read_exact(&mut data).map_err(read_failed)?;
    Ok(Some(data))
}

fn parse_ref_line(line: &[u8]) -> Option<(Vec<u8>, &str, &str)> {
    if line.len() < 41 || line[40] != b' ' {
        return None;
    }
    let oid = hex::decode(&line[..40]).ok()?;
    let rest = std::str::from_utf8(&line[41..])
        .ok()?
        .trim_end_matches('\n');
    let (name, capabilities) = rest.split_once('\0').unwrap_or((rest, ""));
    Some((oid, name.trim(), capabilities))
}

fn head_symref(capabilities: &str) -> Option<String> {
    capabilities
        .split(' ')
        .find_map(|capability| capability.strip_prefix("symref=HEAD:"))
        .map(str::to_owned)
}
