/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod cores;
pub mod model;

pub use cores::{CoreChoice, choose_cores, core_steps, fewer_cores};
pub use model::{GroupKey, Model, Sample};

use crate::score::weights::{RAM_FLOOR_MARGIN, RAM_UPPER_Z, REFERENCE_CORES};
use serde::Serialize;

pub fn log_cores(cores: u32) -> f64 {
    (f64::from(cores.max(1)) / f64::from(REFERENCE_CORES)).ln()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct LogCurve {
    pub at_reference: f64,
    pub per_log_core: f64,
    pub spread: f64,
}

impl LogCurve {
    fn log_at(&self, cores: u32) -> f64 {
        self.at_reference + self.per_log_core * log_cores(cores)
    }

    pub fn median(&self, cores: u32) -> f64 {
        self.log_at(cores).exp()
    }

    pub fn upper(&self, cores: u32) -> f64 {
        (self.log_at(cores) + RAM_UPPER_Z * self.spread).exp()
    }

    fn flat(value: f64) -> Self {
        Self {
            at_reference: value.max(1.0).ln(),
            per_log_core: 0.0,
            spread: 0.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BuildEstimate {
    pub ram: Option<LogCurve>,
    pub build_time: Option<LogCurve>,
    pub output_nar_bytes: Option<u64>,
    pub own_samples: u32,
    pub ram_floor: Vec<(u32, u64)>,
    pub max_cores: Option<u32>,
    pub top_features: Vec<(String, f64)>,
}

pub static NO_ESTIMATE: BuildEstimate = BuildEstimate {
    ram: None,
    build_time: None,
    output_nar_bytes: None,
    own_samples: 0,
    ram_floor: Vec::new(),
    max_cores: None,
    top_features: Vec::new(),
};

impl BuildEstimate {
    pub fn fixed(ram_mb: Option<u64>, build_ms: Option<u64>, own_samples: u32) -> Self {
        Self {
            ram: ram_mb.map(|mb| LogCurve::flat(mb as f64)),
            build_time: build_ms.map(|ms| LogCurve::flat(ms as f64)),
            own_samples,
            ..Self::default()
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded and positive"
    )]
    pub fn ram_mb(&self, cores: u32) -> Option<u64> {
        let predicted = self.ram.map(|curve| curve.upper(cores));
        let floor = self
            .ram_floor
            .iter()
            .filter(|(at, _)| cores >= *at)
            .map(|(_, mb)| *mb as f64 * RAM_FLOOR_MARGIN)
            .reduce(f64::max);
        let mb = match (predicted, floor) {
            (Some(predicted), Some(floor)) => predicted.max(floor),
            (predicted, floor) => predicted.or(floor)?,
        };
        Some(mb.round() as u64)
    }

    pub fn build_ms(&self, cores: u32) -> Option<f64> {
        self.build_time.map(|curve| curve.median(cores))
    }

    pub fn learn(&mut self, fresh: BuildEstimate) {
        let output_nar_bytes = self.output_nar_bytes.or(fresh.output_nar_bytes);
        let mut ram_floor = fresh.ram_floor.clone();
        ram_floor.extend(
            self.ram_floor
                .iter()
                .filter(|f| !fresh.ram_floor.contains(f)),
        );
        *self = BuildEstimate {
            output_nar_bytes,
            ram_floor,
            ..fresh
        };
    }

    pub fn raise_ram_to(&mut self, observed_mb: u64) {
        self.ram_floor.push((1, observed_mb));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_out_of_memory_floor_holds_without_a_fitted_estimate() {
        let killed = BuildEstimate {
            ram_floor: vec![(16, 20_000)],
            ..BuildEstimate::default()
        };

        assert_eq!(killed.ram_mb(16), Some(22_000));
        assert_eq!(killed.ram_mb(64), Some(22_000));
        assert_eq!(killed.ram_mb(8), None);
    }
}
