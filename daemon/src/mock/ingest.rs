/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::mock::spec::Timing;
use crate::mock::timing::chunk_delay;
use tokio::io::{AsyncBufRead, AsyncReadExt as _};

pub async fn read_with_delays<R: AsyncBufRead + Unpin>(
    mut reader: R,
    timing: &Timing,
    key: &str,
) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; timing.chunk_bytes.max(1) as usize];
    let mut index = 0u64;
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Ok(out);
        }

        out.extend_from_slice(&chunk[..n]);
        tokio::time::sleep(chunk_delay(timing, key, index)).await;
        index += 1;
    }
}
