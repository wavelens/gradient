/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::DbErr;
use std::future::Future;

/// Postgres binds at most 65535 parameters per statement. Any `IN (...)` whose
/// element count scales with workload data must stay below this cap; we keep
/// generous headroom for the other binds carried by the same query.
pub const IN_CHUNK_SIZE: usize = 30_000;

/// Run `query` over `ids` split into [`IN_CHUNK_SIZE`] chunks and concatenate
/// the rows. Each id lands in exactly one chunk, so a `WHERE col IN (chunk)`
/// select returns the same rows as one unchunked query, without duplicates.
pub async fn fetch_in_chunks<I, T, F, Fut>(ids: &[I], query: F) -> Result<Vec<T>, DbErr>
where
    I: Clone,
    F: Fn(Vec<I>) -> Fut,
    Fut: Future<Output = Result<Vec<T>, DbErr>>,
{
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    if ids.len() <= IN_CHUNK_SIZE {
        return query(ids.to_vec()).await;
    }

    let mut out = Vec::new();
    for chunk in ids.chunks(IN_CHUNK_SIZE) {
        out.extend(query(chunk.to_vec()).await?);
    }

    Ok(out)
}

/// Run `op` (typically an `update_many`/`delete_many` whose filter binds `ids`)
/// once per [`IN_CHUNK_SIZE`] chunk, discarding each chunk's result. Use this
/// for write statements where the affected-row payload is not needed.
pub async fn for_each_chunk<I, T, F, Fut>(ids: &[I], op: F) -> Result<(), DbErr>
where
    I: Clone,
    F: Fn(Vec<I>) -> Fut,
    Fut: Future<Output = Result<T, DbErr>>,
{
    for chunk in ids.chunks(IN_CHUNK_SIZE) {
        op(chunk.to_vec()).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(fut)
    }

    #[test]
    fn runs_a_single_query_at_or_below_the_cap() {
        run(async {
            let calls = AtomicUsize::new(0);
            let ids: Vec<u32> = (0..IN_CHUNK_SIZE as u32).collect();
            let got = fetch_in_chunks(&ids, |chunk| {
                calls.fetch_add(1, Ordering::SeqCst);
                async move { Ok::<_, DbErr>(chunk) }
            })
            .await
            .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(got, ids);
        });
    }

    #[test]
    fn splits_above_the_cap_and_preserves_every_id() {
        run(async {
            let calls = AtomicUsize::new(0);
            let ids: Vec<u32> = (0..(IN_CHUNK_SIZE as u32 * 2 + 7)).collect();
            let got = fetch_in_chunks(&ids, |chunk| {
                calls.fetch_add(1, Ordering::SeqCst);
                assert!(chunk.len() <= IN_CHUNK_SIZE);
                async move { Ok::<_, DbErr>(chunk) }
            })
            .await
            .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 3);
            assert_eq!(got, ids);
        });
    }

    #[test]
    fn empty_input_runs_no_query() {
        run(async {
            let calls = AtomicUsize::new(0);
            let got = fetch_in_chunks::<u32, u32, _, _>(&[], |chunk| {
                calls.fetch_add(1, Ordering::SeqCst);
                async move { Ok::<_, DbErr>(chunk) }
            })
            .await
            .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(got.is_empty());
        });
    }
}
