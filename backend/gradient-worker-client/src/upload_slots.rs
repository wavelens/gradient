/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The worker's upload slots, handed to a waiting small upload before a waiting
//! large one (see [`gradient_wire::messages::SMALL_UPLOAD_BYTES`]).

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context, Result};
use gradient_util::sync::Mutex;
use tokio::sync::oneshot;

pub(crate) struct SlotPool {
    queue: Mutex<Queue>,
}

struct Queue {
    free: usize,
    small: VecDeque<oneshot::Sender<Slot>>,
    large: VecDeque<oneshot::Sender<Slot>>,
}

/// One held slot; dropping it hands the slot to the next waiter.
pub(crate) struct Slot {
    pool: Option<Arc<SlotPool>>,
}

impl SlotPool {
    pub(crate) fn new(slots: usize) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(Queue {
                free: slots,
                small: VecDeque::new(),
                large: VecDeque::new(),
            }),
        })
    }

    pub(crate) async fn acquire(self: &Arc<Self>, small: bool) -> Result<Slot> {
        let waiting = {
            let mut queue = self.queue.lock();
            if queue.free > 0 {
                queue.free -= 1;
                return Ok(Slot {
                    pool: Some(Arc::clone(self)),
                });
            }
            let (tx, rx) = oneshot::channel();
            if small {
                queue.small.push_back(tx);
            } else {
                queue.large.push_back(tx);
            }
            rx
        };
        waiting.await.context("upload slots closed")
    }

    fn release(self: &Arc<Self>) {
        let mut queue = self.queue.lock();
        let mut slot = Slot {
            pool: Some(Arc::clone(self)),
        };
        loop {
            let Some(waiter) = queue.small.pop_front().or_else(|| queue.large.pop_front()) else {
                slot.pool = None;
                queue.free += 1;
                return;
            };
            match waiter.send(slot) {
                Ok(()) => return,
                Err(unclaimed) => slot = unclaimed,
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            pool.release();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_waiting_small_upload_takes_the_slot_before_an_earlier_large_one() {
        let pool = SlotPool::new(1);
        let held = pool.acquire(false).await.unwrap();
        let large = tokio::spawn({
            let pool = Arc::clone(&pool);
            async move { pool.acquire(false).await.map(drop) }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let small = tokio::spawn({
            let pool = Arc::clone(&pool);
            async move { pool.acquire(true).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        drop(held);
        let small_slot = tokio::time::timeout(Duration::from_secs(1), small)
            .await
            .expect("the small upload is served first")
            .unwrap()
            .unwrap();
        assert!(!large.is_finished(), "the large one still waits");
        drop(small_slot);
        tokio::time::timeout(Duration::from_secs(1), large)
            .await
            .expect("then the large one")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn a_waiter_that_gave_up_passes_the_slot_on() {
        let pool = SlotPool::new(1);
        let held = pool.acquire(false).await.unwrap();
        let abandoned = tokio::spawn({
            let pool = Arc::clone(&pool);
            async move { pool.acquire(true).await.map(drop) }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        abandoned.abort();
        let _ = abandoned.await;

        drop(held);
        tokio::time::timeout(Duration::from_secs(1), pool.acquire(false))
            .await
            .expect("the slot came back")
            .unwrap();
    }
}
