/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[allow(dead_code, reason = "the vsock listener reads it once it exists")]
mod output;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    serve().await
}

async fn serve() -> Result<()> {
    Ok(())
}
