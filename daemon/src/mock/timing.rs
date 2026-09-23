/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::mock::spec::{Dist, Node, Timing};
use std::time::Duration;

const Z_P99: f64 = 2.326_347_874;

pub fn next(mut h: u64) -> u64 {
    h = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

pub fn key_hash(seed: u64, key: &[&str]) -> u64 {
    key.iter().fold(next(seed), |h, part| {
        part.bytes()
            .fold(next(h ^ 0xff), |h, b| next(h ^ u64::from(b)))
    })
}

fn unit(h: u64) -> f64 {
    ((h >> 11) as f64 + 0.5) / (1u64 << 53) as f64
}

pub fn draw_ms(dist: &Dist, seed: u64, key: &[&str]) -> f64 {
    let h = key_hash(seed, key);
    match *dist {
        Dist::Fixed { ms } => ms,
        Dist::Uniform { min, max } => min + unit(h) * (max - min),
        Dist::Lognormal { median, p99 } => {
            let sigma = (p99 / median).ln() / Z_P99;
            let z = (-2.0 * unit(h).ln()).sqrt() * (std::f64::consts::TAU * unit(next(h))).cos();
            median * (sigma * z).exp()
        }
    }
}

fn scaled(ms: f64, scale: f64) -> Duration {
    Duration::from_secs_f64((ms * scale).max(0.0) / 1000.0)
}

pub fn build_delay(node: &Node, attempt: u32) -> Duration {
    let ms = node.build.duration_ms.map_or_else(
        || {
            draw_ms(
                &node.timing.build_ms,
                node.timing.seed,
                &[&node.drv_path, &attempt.to_string()],
            )
        },
        |ms| ms as f64,
    );
    scaled(ms, node.timing.scale)
}

pub fn chunk_delay(timing: &Timing, path: &str, index: u64) -> Duration {
    scaled(
        draw_ms(&timing.chunk_ms, timing.seed, &[path, &index.to_string()]),
        timing.scale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_are_stable_for_a_seed() {
        let dist = Dist::Lognormal {
            median: 40.0,
            p99: 800.0,
        };
        assert_eq!(
            draw_ms(&dist, 7, &["a", "1"]),
            draw_ms(&dist, 7, &["a", "1"])
        );
        assert_ne!(
            draw_ms(&dist, 7, &["a", "1"]),
            draw_ms(&dist, 8, &["a", "1"])
        );
    }

    #[test]
    fn lognormal_matches_median_and_p99() {
        let dist = Dist::Lognormal {
            median: 40.0,
            p99: 800.0,
        };
        let mut draws: Vec<f64> = (0..20_000)
            .map(|i| draw_ms(&dist, 1, &[&i.to_string()]))
            .collect();
        draws.sort_by(f64::total_cmp);
        let median = draws[10_000];
        let p99 = draws[19_800];
        assert!((36.0..44.0).contains(&median), "median {median}");
        assert!((640.0..960.0).contains(&p99), "p99 {p99}");
    }

    #[test]
    fn uniform_stays_in_bounds() {
        let dist = Dist::Uniform { min: 1.0, max: 3.0 };
        assert!((0..1000).all(|i| (1.0..=3.0).contains(&draw_ms(&dist, 1, &[&i.to_string()]))));
    }
}
