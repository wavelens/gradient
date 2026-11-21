/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use futures::StreamExt as _;
use gradient_storage::nar_extract::nar_reader_from_stream;
use gradient_storage::{NarSource, NarStore};
use tokio::io::{AsyncBufRead, BufReader};

pub async fn open_raw(
    nar_storage: &NarStore,
    nar_object_storage_key: &str,
) -> anyhow::Result<Option<impl AsyncBufRead + Send + Unpin + use<>>> {
    let Some(source) = nar_storage.open(nar_object_storage_key, 0).await? else {
        return Ok(None);
    };

    let stream = match source {
        NarSource::Hot(bytes) => futures::stream::once(async move { Ok(bytes) }).boxed(),
        NarSource::Stream { stream, .. } => stream,
    };

    Ok(Some(BufReader::new(nar_reader_from_stream(stream))))
}
