/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Mutex;

use super::lock;

/// Running requests report thunks since the request start. Final deltas replace the last tick.
#[derive(Debug, Default)]
pub struct LiveThunks {
    state: Mutex<(u64, HashMap<u32, u64>)>,
}

impl LiveThunks {
    pub fn tick(&self, pid: u32, thunks: u64) {
        lock(&self.state).1.insert(pid, thunks);
    }

    pub fn commit(&self, pid: u32, thunks: u64) {
        let mut state = lock(&self.state);
        state.1.remove(&pid);
        state.0 += thunks;
    }

    pub fn forget(&self, pid: u32) {
        lock(&self.state).1.remove(&pid);
    }

    pub fn total(&self) -> u64 {
        let state = lock(&self.state);
        state.0 + state.1.values().sum::<u64>()
    }

    pub fn reset(&self) {
        *lock(&self.state) = Default::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_final_delta_replaces_a_workers_ticks() {
        let live = LiveThunks::default();
        live.tick(1, 100);
        live.tick(2, 40);
        live.tick(1, 180);
        assert_eq!(live.total(), 220);
        live.commit(1, 200);
        assert_eq!(live.total(), 240);
        live.forget(2);
        assert_eq!(live.total(), 200);
        live.reset();
        assert_eq!(live.total(), 0);
    }
}
