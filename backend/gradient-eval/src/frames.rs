/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::Write;
use std::sync::{Mutex, MutexGuard, PoisonError};

use nix_bindings::EvalStats;

use crate::ipc::{EvalResponse, encode_response, write_frame};
use crate::stats::StatsDelta;

/// One lock orders every frame, so a stats tick can never land after its request's final response.
pub(crate) struct Frames<W> {
    state: Mutex<FrameState<W>>,
}

struct FrameState<W> {
    out: W,
    last: EvalStats,
    in_request: bool,
}

impl<W: Write> Frames<W> {
    pub(crate) fn new(out: W, last: EvalStats) -> Self {
        Self {
            state: Mutex::new(FrameState {
                out,
                last,
                in_request: false,
            }),
        }
    }

    pub(crate) fn begin(&self) {
        self.lock().in_request = true;
    }

    pub(crate) fn end(&self, now: Option<EvalStats>) -> Option<StatsDelta> {
        let mut state = self.lock();
        state.in_request = false;
        let now = now?;
        let delta = StatsDelta::between(&now, &state.last);
        state.last = now;
        Some(delta)
    }

    pub(crate) fn tick(&self, now: EvalStats) -> std::io::Result<()> {
        let mut state = self.lock();
        if !state.in_request {
            return Ok(());
        }
        let delta = StatsDelta::between(&now, &state.last);
        write_response(&mut state.out, &EvalResponse::Stats { delta })
    }

    pub(crate) fn send(&self, resp: &EvalResponse) -> std::io::Result<()> {
        write_response(&mut self.lock().out, resp)
    }

    #[cfg(test)]
    fn into_output(self) -> W {
        self.state
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .out
    }

    fn lock(&self) -> MutexGuard<'_, FrameState<W>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn write_response<W: Write>(out: &mut W, resp: &EvalResponse) -> std::io::Result<()> {
    let payload = encode_response(resp).map_err(std::io::Error::other)?;
    write_frame(out, &payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{decode_response, read_frame};

    fn stats(thunks: u64) -> EvalStats {
        EvalStats {
            nr_thunks: thunks,
            ..Default::default()
        }
    }

    fn written(frames: Frames<Vec<u8>>) -> Vec<EvalResponse> {
        let bytes = frames.into_output();
        let mut cursor = std::io::Cursor::new(bytes);
        std::iter::from_fn(|| read_frame(&mut cursor).unwrap())
            .map(|f| decode_response(&f).unwrap())
            .collect()
    }

    #[test]
    fn no_tick_outside_a_request() {
        let frames = Frames::new(Vec::new(), stats(10));
        frames.tick(stats(50)).unwrap();
        assert!(written(frames).is_empty());
    }

    #[test]
    fn a_tick_reports_the_request_so_far_without_moving_its_baseline() {
        let frames = Frames::new(Vec::new(), stats(10));
        frames.begin();
        frames.tick(stats(30)).unwrap();
        frames.tick(stats(45)).unwrap();
        let end = frames.end(Some(stats(60)));
        assert_eq!(end.map(|d| d.nr_thunks), Some(50));
        let thunks: Vec<u64> = written(frames)
            .into_iter()
            .map(|r| match r {
                EvalResponse::Stats { delta } => delta.nr_thunks,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(thunks, vec![20, 35]);
    }

    #[test]
    fn no_tick_after_the_request_ends() {
        let frames = Frames::new(Vec::new(), stats(0));
        frames.begin();
        frames.end(Some(stats(5)));
        frames.tick(stats(9)).unwrap();
        assert!(written(frames).is_empty());
    }

    #[test]
    fn a_request_without_metrics_keeps_the_baseline() {
        let frames = Frames::new(Vec::new(), stats(10));
        assert_eq!(frames.end(None).map(|d| d.nr_thunks), None);
        frames.begin();
        assert_eq!(frames.end(Some(stats(15))).map(|d| d.nr_thunks), Some(5));
    }
}
