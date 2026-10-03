/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use gradient_util::sync::Mutex;

/// Running requests report thunks since their start. A request's final delta replaces its last tick.
#[derive(Debug, Default)]
pub(crate) struct LiveThunks {
    state: Mutex<(u64, HashMap<u32, u64>)>,
}

impl LiveThunks {
    pub(crate) fn tick(&self, pid: u32, thunks: u64) {
        self.state.lock().1.insert(pid, thunks);
    }

    pub(crate) fn commit(&self, pid: u32, thunks: u64) {
        let mut state = self.state.lock();
        state.1.remove(&pid);
        state.0 += thunks;
    }

    pub(crate) fn forget(&self, pid: u32) {
        self.state.lock().1.remove(&pid);
    }

    pub(crate) fn total(&self) -> u64 {
        let state = self.state.lock();
        state.0 + state.1.values().sum::<u64>()
    }

    pub(crate) fn reset(&self) {
        *self.state.lock() = Default::default();
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
