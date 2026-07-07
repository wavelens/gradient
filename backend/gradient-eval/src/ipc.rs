/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Wire protocol between the worker parent and its eval-worker subprocesses.
//!
//! Frames are `u32` little-endian payload length + rkyv bytes, following the
//! main gradient protocol's rkyv conventions (rancor errors, 16-byte realign
//! before decode) over a pipe instead of WebSocket message boundaries. The
//! subprocess announces [`EVAL_IPC_VERSION`] as a single raw byte before its
//! first frame so a parent never talks a stale binary's dialect.
//!
//! `Resolve` streams: the subprocess answers with one [`EvalResponse::ResolveItem`]
//! per attr as soon as it is resolved, terminated by [`EvalResponse::ResolveEnd`].
//! Every other request is strictly one request, one response. Streaming means a
//! subprocess crash mid-batch only loses the attrs not yet streamed, so the
//! parent can isolate the crasher precisely instead of bisecting the batch.
//!
//! Types keep serde derives alongside rkyv: JSON is never on the subprocess
//! wire, but the worker's `--eval-driver` test harness and debug logging speak
//! it, and the serde tags are the protocol's readable documentation.

use rkyv::rancor::Error as RkyvError;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

use crate::stats::StatsDelta;

/// Bumped whenever the frame layout or the rkyv shape of the types below
/// changes. Parent and subprocess are the same re-exec'd binary, so a mismatch
/// only happens when the binary is replaced mid-run; the handshake turns that
/// from undecodable frames into one clear error.
pub const EVAL_IPC_VERSION: u8 = 2;

/// Upper bound on a single frame's payload. Far above any real message (a
/// discovery response for a huge flake is a few MiB); its job is to turn a
/// corrupted length prefix into an immediate error instead of a giant alloc.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

/// Request from parent to worker, one frame each.
#[derive(Debug, Clone, Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EvalRequest {
    /// Split `wildcards` into disjoint sub-patterns (one per first-wildcard
    /// child) so the parent can fan discovery across the pool, one shard per
    /// system within each worker's memory budget.
    Plan {
        repository: String,
        wildcards: Vec<String>,
    },
    /// Discover all attribute paths in `repository` matching `wildcards`.
    List {
        repository: String,
        wildcards: Vec<String>,
    },
    /// Resolve a batch of attribute paths to `(drv_path, references)` tuples.
    /// Answered by a `ResolveItem` stream terminated with `ResolveEnd`;
    /// per-attr failures ride inside their item, not as a top-level `Err`.
    Resolve {
        repository: String,
        attrs: Vec<String>,
    },
    /// Return `repository`'s eval-cache fingerprint without evaluating it.
    /// `None` in the response for mutable/dirty flakes.
    Fingerprint { repository: String },
    /// Fold the eval-cache WAL into the main `.sqlite` (truncate checkpoint).
    /// Run once after all shards finish, before the fleet-share push.
    Checkpoint { repository: String },
    /// Ask the worker to exit cleanly. Parent uses this on graceful shutdown.
    Shutdown,
}

/// Response from worker to parent, one frame each.
#[derive(Debug, Clone, Archive, RkyvSerialize, RkyvDeserialize, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalResponse {
    PlanOk {
        sub_patterns: Vec<String>,
        errors: Vec<String>,
    },
    ListOk {
        attrs: Vec<String>,
        warnings: Vec<String>,
        errors: Vec<String>,
        stats: Option<StatsDelta>,
    },
    /// One resolved attr of an in-flight `Resolve`, streamed in request order.
    ResolveItem { item: ResolvedItem },
    /// Terminates a `Resolve` stream, carrying the batch-wide leftovers.
    ResolveEnd {
        warnings: Vec<String>,
        stats: Option<StatsDelta>,
    },
    FingerprintOk {
        fingerprint: Option<String>,
    },
    CheckpointOk,
    Err {
        message: String,
    },
}

/// One streamed element of a `Resolve`. Either `drv_path` is set (success)
/// or `error` is set (failure for that one attr).
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
    rkyv::from_bytes::<EvalRequest, RkyvError>(&realign(bytes))
}

pub fn decode_response(bytes: &[u8]) -> Result<EvalResponse, RkyvError> {
    rkyv::from_bytes::<EvalResponse, RkyvError>(&realign(bytes))
}

/// rkyv validation requires its archive-aligned input; bytes that crossed the
/// pipe land at whatever alignment the reader's buffer had, so copy them into
/// an [`AlignedVec`] first (same rule as the main protocol's wire decode).
fn realign(bytes: &[u8]) -> AlignedVec {
    let mut aligned = AlignedVec::with_capacity(bytes.len());
    aligned.extend_from_slice(bytes);
    aligned
}

/// Write one length-prefixed frame and flush, so a streamed item is visible to
/// the parent even if the subprocess dies on the very next attr.
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

/// Read one frame. `Ok(None)` on clean EOF at a frame boundary (the peer
/// closed the pipe between messages); an EOF inside a frame is an error.
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

    fn roundtrip_request(req: &EvalRequest) -> EvalRequest {
        decode_request(&encode_request(req).unwrap()).unwrap()
    }

    fn roundtrip_response(resp: &EvalResponse) -> EvalResponse {
        decode_response(&encode_response(resp).unwrap()).unwrap()
    }

    #[test]
    fn requests_roundtrip_through_rkyv() {
        let requests = [
            EvalRequest::Plan {
                repository: "github:nixos/nixpkgs".into(),
                wildcards: vec!["packages.*.*".into()],
            },
            EvalRequest::List {
                repository: "github:nixos/nixpkgs".into(),
                wildcards: vec!["packages.*.*".into(), "!packages.x.broken".into()],
            },
            EvalRequest::Resolve {
                repository: "github:nixos/nixpkgs".into(),
                attrs: vec!["packages.x86_64-linux.hello".into()],
            },
            EvalRequest::Fingerprint {
                repository: "github:nixos/nixpkgs".into(),
            },
            EvalRequest::Checkpoint {
                repository: "github:nixos/nixpkgs".into(),
            },
            EvalRequest::Shutdown,
        ];

        for req in &requests {
            assert_eq!(
                format!("{:?}", roundtrip_request(req)),
                format!("{req:?}"),
                "rkyv roundtrip mismatch"
            );
        }
    }

    #[test]
    fn responses_roundtrip_through_rkyv() {
        let responses = [
            EvalResponse::PlanOk {
                sub_patterns: vec!["packages.x86_64-linux.#".into()],
                errors: vec![],
            },
            EvalResponse::ListOk {
                attrs: vec!["packages.x86_64-linux.hello".into()],
                warnings: vec!["warning: insecure".into()],
                errors: vec!["failed to evaluate 'packages.x86_64-linux.broken': boom".into()],
                stats: Some(StatsDelta {
                    nr_thunks: 7,
                    gc_heap_size: 42,
                    ..Default::default()
                }),
            },
            EvalResponse::ResolveItem {
                item: ResolvedItem {
                    attr: "packages.x86_64-linux.hello".into(),
                    drv_path: Some("aaaa-hello.drv".into()),
                    references: vec!["bbbb-dep".into()],
                    error: None,
                },
            },
            EvalResponse::ResolveEnd {
                warnings: vec![],
                stats: None,
            },
            EvalResponse::FingerprintOk {
                fingerprint: Some("deadbeef".into()),
            },
            EvalResponse::CheckpointOk,
            EvalResponse::Err {
                message: "something went wrong".into(),
            },
        ];

        for resp in &responses {
            assert_eq!(
                format!("{:?}", roundtrip_response(resp)),
                format!("{resp:?}"),
                "rkyv roundtrip mismatch"
            );
        }
    }

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
    fn decode_survives_misaligned_input() {
        // Simulate an arbitrary-alignment read buffer by shifting the payload
        // one byte inside a larger allocation.
        let bytes = encode_request(&EvalRequest::Shutdown).unwrap();
        let mut shifted = vec![0u8; bytes.len() + 1];
        shifted[1..].copy_from_slice(&bytes);
        assert!(decode_request(&shifted[1..]).is_ok());
    }
}
