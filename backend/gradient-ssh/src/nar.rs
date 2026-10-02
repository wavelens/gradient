/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use futures::StreamExt as _;
use gradient_core::ServerState;
use gradient_storage::NarSource;
use gradient_storage::nar_extract::nar_reader_from_stream;
use tokio::io::{AsyncBufRead, BufReader};

pub async fn open_raw(
    state: &ServerState,
    hash: &str,
) -> anyhow::Result<Option<impl AsyncBufRead + Send + Unpin + use<>>> {
    let Some(source) = state.nar_storage.open(hash, 0).await? else {
        return Ok(None);
    };

    let stream = match source {
        NarSource::Hot(bytes) => futures::stream::once(async move { Ok(bytes) }).boxed(),
        NarSource::Stream { stream, .. } => stream,
    };

    Ok(Some(BufReader::new(nar_reader_from_stream(stream))))
}
