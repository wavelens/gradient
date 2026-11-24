/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

/// Worker push and server-side source materialisation must agree on this level. Every `nars/`
/// object is then encoded uniformly.
pub const NAR_ZSTD_LEVEL: i32 = 6;
pub const BULK_CHUNK_SIZE: usize = 512 * 1024;
pub const PRESIGN_TTL: Duration = Duration::from_secs(3600);
/// NARs above this size use a presigned multipart upload. Single presigned PUT requests hold the
/// whole compressed NAR in worker memory. Workers resend the full compressed NAR after a failure.
/// Multipart uploads keep about two parts in memory and retry each part.
pub const MULTIPART_NAR_BYTES: u64 = 256 * 1024 * 1024;
pub const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(120);
