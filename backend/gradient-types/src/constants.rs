/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

pub const MAX_BUILD_REQUEST_SIZE: usize = 20 * 1024 * 1024;
pub const UPLOAD_SESSION_TTL: Duration = Duration::from_secs(3600);

/// zstd level for every NAR packed or repacked (worker push and server-side
/// source materialisation must agree so `nars/` objects are encoded uniformly).
pub const NAR_ZSTD_LEVEL: i32 = 6;
/// zstd level for on-the-fly directory-extract tarballs (cheap, CPU-light).
pub const TAR_ZSTD_LEVEL: i32 = 1;
/// zstd level for finalized build-log chunks (0 = zstd default).
pub const LOG_CHUNK_ZSTD_LEVEL: i32 = 0;
/// Cap on per-file buffer preallocation during NAR extraction (16 MiB).
pub const NAR_EXTRACT_MAX_PREALLOC: usize = 16 * 1024 * 1024;
/// Lifetime of presigned GET/PUT URLs handed to workers and cache clients.
pub const PRESIGN_TTL: Duration = Duration::from_secs(3600);
