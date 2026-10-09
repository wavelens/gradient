/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use async_stream::stream;
use futures::{Stream, StreamExt, pin_mut};
use std::time::Duration;

pub const KEEPALIVE: Duration = Duration::from_secs(15);
pub const MAX_CHUNK_BYTES: usize = 256 * 1024;

/// Idle proxies cut a silent stream, and the client drops a chunk above its line cap.
pub fn keepalive_chunks<S>(inner: S) -> impl Stream<Item = String>
where
    S: Stream<Item = String>,
{
    stream! {
        pin_mut!(inner);
        loop {
            match tokio::time::timeout(KEEPALIVE, inner.next()).await {
                Ok(Some(chunk)) => {
                    for piece in split_chunk(chunk) {
                        yield piece;
                    }
                }
                Ok(None) => break,
                Err(_) => yield String::new(),
            }
        }
    }
}

pub fn split_chunk(chunk: String) -> Vec<String> {
    if chunk.len() <= MAX_CHUNK_BYTES {
        return vec![chunk];
    }

    let mut pieces = Vec::new();
    let mut rest = chunk.as_str();
    while rest.len() > MAX_CHUNK_BYTES {
        let cut = rest[..MAX_CHUNK_BYTES]
            .rfind('\n')
            .map(|at| at + 1)
            .unwrap_or_else(|| char_boundary_at_or_below(rest, MAX_CHUNK_BYTES));
        pieces.push(rest[..cut].to_owned());
        rest = &rest[cut..];
    }
    if !rest.is_empty() {
        pieces.push(rest.to_owned());
    }

    pieces
}

fn char_boundary_at_or_below(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_above_the_cap_is_split_at_line_ends_without_loss() {
        let line = format!("{}\n", "x".repeat(1000));
        let chunk = line.repeat(700);

        let pieces = split_chunk(chunk.clone());

        assert!(pieces.len() > 1);
        assert!(pieces.iter().all(|p| p.len() <= MAX_CHUNK_BYTES));
        assert!(pieces[..pieces.len() - 1].iter().all(|p| p.ends_with('\n')));
        assert_eq!(pieces.concat(), chunk);
    }

    #[test]
    fn a_single_line_above_the_cap_is_split_on_char_boundaries() {
        let chunk = "ä".repeat(MAX_CHUNK_BYTES);

        let pieces = split_chunk(chunk.clone());

        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces.concat(), chunk);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_stream_sends_empty_keepalives_and_ends_with_its_source() {
        let (tx, rx) = futures::channel::mpsc::unbounded::<String>();
        let out = keepalive_chunks(rx);
        pin_mut!(out);

        tx.unbounded_send("a> first\n".into()).unwrap();
        assert_eq!(out.next().await.as_deref(), Some("a> first\n"));

        let quiet_sender = async move {
            tokio::time::sleep(KEEPALIVE * 2 + Duration::from_secs(1)).await;
            tx.unbounded_send("a> second\n".into()).unwrap();
            drop(tx);
        };
        let rest = async {
            let mut items = Vec::new();
            while let Some(item) = out.next().await {
                items.push(item);
            }
            items
        };
        let ((), items) = tokio::join!(quiet_sender, rest);

        assert_eq!(items, ["", "", "a> second\n"]);
    }
}
