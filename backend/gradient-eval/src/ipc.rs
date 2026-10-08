/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use rkyv::rancor::Error as RkyvError;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

use crate::stats::StatsDelta;

/// The version must be bumped whenever the frame layout or a type's rkyv shape changes.
/// A mismatch can only happen when the binary is replaced mid-run.
pub const EVAL_IPC_VERSION: u8 = 10;

pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Archive,
    RkyvSerialize,
    RkyvDeserialize,
    Serialize,
    Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct AttrError {
    pub attr: String,
    pub message: String,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct DiscoveryShard {
    pub pattern: String,
    #[serde(default)]
    pub only: Option<Vec<String>>,
}

#[derive(Debug, Clone, Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EvalRequest {
    Plan {
        repository: String,
        wildcards: Vec<String>,
        #[serde(default)]
        input_overrides: Vec<(String, String)>,
    },
    List {
        repository: String,
        wildcards: Vec<String>,
        #[serde(default)]
        only: Option<Vec<String>>,
        #[serde(default)]
        input_overrides: Vec<(String, String)>,
    },
    Fingerprint {
        repository: String,
        #[serde(default)]
        input_overrides: Vec<(String, String)>,
    },
    Checkpoint {
        repository: String,
        #[serde(default)]
        input_overrides: Vec<(String, String)>,
    },
    FetchInput {
        locked: String,
        #[serde(default)]
        git_ssh_command: Option<String>,
    },
    BuildDone {
        #[serde(default)]
        error: Option<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalResponse {
    PlanOk {
        shards: Vec<DiscoveryShard>,
        errors: Vec<AttrError>,
    },
    ListOk {
        items: Vec<ResolvedItem>,
        #[serde(default)]
        deferred: Vec<DiscoveryShard>,
        warnings: Vec<String>,
        errors: Vec<AttrError>,
        stats: Option<StatsDelta>,
    },
    FingerprintOk {
        fingerprint: Option<String>,
    },
    CheckpointOk,
    FetchOk {
        store_path: String,
    },
    Stats {
        delta: StatsDelta,
    },
    NeedsBuild {
        derived_paths: Vec<String>,
    },
    Err {
        message: String,
    },
}

#[derive(Debug, Clone, Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
pub struct ResolvedItem {
    pub attr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drv_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn encode_request(req: &EvalRequest) -> Result<AlignedVec, RkyvError> {
    rkyv::to_bytes::<RkyvError>(req)
}

pub fn encode_response(resp: &EvalResponse) -> Result<AlignedVec, RkyvError> {
    rkyv::to_bytes::<RkyvError>(resp)
}

pub fn decode_request(bytes: &[u8]) -> Result<EvalRequest, RkyvError> {
    rkyv::from_bytes::<EvalRequest, RkyvError>(bytes)
}

pub fn decode_response(bytes: &[u8]) -> Result<EvalResponse, RkyvError> {
    rkyv::from_bytes::<EvalResponse, RkyvError>(bytes)
}

pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|&l| l <= MAX_FRAME_BYTES)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("frame of {} bytes exceeds MAX_FRAME_BYTES", payload.len()),
            )
        })?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

pub fn read_frame<R: Read>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }

    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds MAX_FRAME_BYTES (corrupt stream?)"),
        ));
    }

    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip_and_eof_between_frames_is_clean() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"first").unwrap();
        write_frame(&mut buf, b"").unwrap();

        let mut r = std::io::Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap().as_deref(), Some(&b"first"[..]));
        assert_eq!(read_frame(&mut r).unwrap().as_deref(), Some(&b""[..]));
        assert!(read_frame(&mut r).unwrap().is_none(), "clean EOF is None");
    }

    #[test]
    fn truncated_frame_is_an_error_not_eof() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"payload").unwrap();
        buf.truncate(buf.len() - 2);

        let mut r = std::io::Cursor::new(buf);
        assert!(read_frame(&mut r).is_err(), "EOF inside a frame must error");
    }

    #[test]
    fn oversized_length_prefix_is_rejected() {
        let mut buf = (MAX_FRAME_BYTES + 1).to_le_bytes().to_vec();
        buf.extend_from_slice(&[0u8; 8]);
        let mut r = std::io::Cursor::new(buf);
        assert!(read_frame(&mut r).is_err());
    }

    #[test]
    fn a_fetch_request_and_its_frames_round_trip() {
        let req = EvalRequest::FetchInput {
            locked: r#"{"type":"github","owner":"o","repo":"r","rev":"abc","narHash":"sha256-x"}"#
                .into(),
            git_ssh_command: Some("ssh -i key".into()),
        };
        let back = decode_request(&encode_request(&req).unwrap()).unwrap();
        assert!(
            matches!(back, EvalRequest::FetchInput { ref locked, ref git_ssh_command }
            if locked.contains("github") && git_ssh_command.as_deref() == Some("ssh -i key"))
        );

        let stats = EvalResponse::Stats {
            delta: StatsDelta {
                nr_thunks: 7,
                ..Default::default()
            },
        };
        let back = decode_response(&encode_response(&stats).unwrap()).unwrap();
        assert!(matches!(back, EvalResponse::Stats { delta } if delta.nr_thunks == 7));
    }

    #[test]
    fn needs_build_and_build_done_round_trip() {
        let paths = vec!["/nix/store/aaaa-src.drv^out".to_string()];
        let needs = EvalResponse::NeedsBuild {
            derived_paths: paths.clone(),
        };
        let back = decode_response(&encode_response(&needs).unwrap()).unwrap();
        assert!(
            matches!(back, EvalResponse::NeedsBuild { derived_paths } if derived_paths == paths)
        );

        let done = EvalRequest::BuildDone {
            error: Some("build b1 failed".into()),
        };
        let back = decode_request(&encode_request(&done).unwrap()).unwrap();
        assert!(
            matches!(back, EvalRequest::BuildDone { error } if error.as_deref() == Some("build b1 failed"))
        );

        let json = serde_json::to_value(EvalRequest::BuildDone { error: None }).unwrap();
        assert_eq!(json["op"], "build_done");
        let json = serde_json::to_value(&needs).unwrap();
        assert_eq!(json["kind"], "needs_build");
    }

    #[test]
    fn decode_survives_misaligned_input() {
        let bytes = encode_request(&EvalRequest::Shutdown).unwrap();
        let mut shifted = vec![0u8; bytes.len() + 1];
        shifted[1..].copy_from_slice(&bytes);
        assert!(decode_request(&shifted[1..]).is_ok());
    }
}
