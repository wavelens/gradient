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
/// A single S3 PUT is capped at 5 GiB. NARs above this size are going through a presigned multipart
/// upload.
pub const MULTIPART_NAR_BYTES: u64 = 1024 * 1024 * 1024;
