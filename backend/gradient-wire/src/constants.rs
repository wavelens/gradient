/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

/// zstd level for every NAR packed or repacked (worker push and server-side
/// source materialisation must agree so `nars/` objects are encoded uniformly).
pub const NAR_ZSTD_LEVEL: i32 = 6;
/// Payload size of one bulk frame (NAR and eval-cache chunks, both directions)
/// and the unit at which a control reply can preempt a transfer.
pub const BULK_CHUNK_SIZE: usize = 512 * 1024;
/// Lifetime of presigned GET/PUT URLs handed to workers and cache clients.
pub const PRESIGN_TTL: Duration = Duration::from_secs(3600);
/// NARs above this size are pushed as a presigned multipart upload, since a
/// single S3 PUT is capped at 5 GiB.
pub const MULTIPART_NAR_BYTES: u64 = 1024 * 1024 * 1024;
