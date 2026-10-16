/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use gradient_types::ids::DerivationId;

use super::{BuildEstimate, LogCurve, log_cores};
use crate::score::weights::{
    BUILD_TIME_CORE_SLOPE_PRIOR, CORE_FEATURE_PENALTY, CORE_SLOPE_PENALTY, FEATURE_PENALTY,
    FIT_EPOCHS, MIN_SPREAD, OWN_SAMPLE_PRIOR_WEIGHT, RAM_CORE_SLOPE_PRIOR, REFERENCE_CORES,
    TOP_FEATURES,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Target {
    Ram,
    BuildTime,
    OutputNar,
}

impl Target {
    const ALL: [Self; 3] = [Self::Ram, Self::BuildTime, Self::OutputNar];

    const fn core_slope_prior(self) -> Option<f64> {
        match self {
            Self::Ram => Some(RAM_CORE_SLOPE_PRIOR),
            Self::BuildTime => Some(BUILD_TIME_CORE_SLOPE_PRIOR),
            Self::OutputNar => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub derivation: DerivationId,
    pub architecture: String,
    pub pname: String,
    pub input_pnames: Vec<String>,
    pub cores: Option<u32>,
    pub peak_ram_mb: Option<u64>,
    pub build_ms: Option<f64>,
    pub output_nar_bytes: Option<u64>,
    pub lower_bound: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GroupKey {
    pub architecture: String,
    pub pname: String,
    pub input_pnames: Vec<String>,
}

impl GroupKey {
    fn features(&self) -> Vec<String> {
        std::iter::once(format!("pname:{}", self.pname))
            .chain(self.input_pnames.iter().map(|i| format!("input:{i}")))
            .collect()
    }
}

impl Sample {
    pub fn group(&self) -> GroupKey {
        GroupKey {
            architecture: self.architecture.clone(),
            pname: self.pname.clone(),
            input_pnames: self.input_pnames.clone(),
        }
    }

    fn value(&self, target: Target) -> Option<f64> {
        let raw = match target {
            Target::Ram => self.peak_ram_mb.map(|mb| mb as f64),
            Target::BuildTime if !self.lower_bound => self.build_ms,
            Target::OutputNar if !self.lower_bound => self.output_nar_bytes.map(|b| b as f64),
            Target::BuildTime | Target::OutputNar => None,
        };
        raw.filter(|v| *v > 0.0).map(f64::ln)
    }

    fn censored(&self, target: Target) -> bool {
        target == Target::Ram && self.lower_bound
    }

    fn log_cores(&self, target: Target) -> f64 {
        match (target.core_slope_prior(), self.cores) {
            (Some(_), Some(cores)) => log_cores(cores),
            _ => 0.0,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct LinearFit {
    bias: f64,
    core_slope: f64,
    weights: HashMap<String, f64>,
    core_weights: HashMap<String, f64>,
    error: f64,
}

impl LinearFit {
    fn curve(&self, features: &[String]) -> LogCurve {
        let sum =
            |map: &HashMap<String, f64>| features.iter().filter_map(|f| map.get(f)).sum::<f64>();
        LogCurve {
            at_reference: self.bias + sum(&self.weights),
            per_log_core: self.core_slope + sum(&self.core_weights),
            spread: self.error,
        }
    }

    fn top_features(&self, features: &[String]) -> Vec<(String, f64)> {
        let mut weighed: Vec<(String, f64)> = features
            .iter()
            .filter_map(|f| self.weights.get(f).map(|w| (f.clone(), *w)))
            .collect();
        weighed.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
        weighed.truncate(TOP_FEATURES);
        weighed
    }
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    fits: HashMap<(String, Target), LinearFit>,
}

impl Model {
    pub fn fit(samples: &[Sample]) -> Self {
        let mut by_architecture: HashMap<&str, Vec<&Sample>> = HashMap::new();
        for sample in samples {
            by_architecture
                .entry(&sample.architecture)
                .or_default()
                .push(sample);
        }
        let mut fits = HashMap::new();
        for (architecture, samples) in by_architecture {
            for target in Target::ALL {
                if let Some(fit) = Fitter::new(&samples, target).map(Fitter::fit) {
                    fits.insert((architecture.to_owned(), target), fit);
                }
            }
        }
        Self { fits }
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded and positive"
    )]
    pub fn estimate(&self, key: &GroupKey, own: &[&Sample]) -> BuildEstimate {
        let features = key.features();
        let fit = |target: Target| self.fits.get(&(key.architecture.clone(), target));
        let curve = |target: Target| Some(blend(fit(target)?.curve(&features), own, target));
        BuildEstimate {
            ram: curve(Target::Ram),
            build_time: curve(Target::BuildTime),
            output_nar_bytes: curve(Target::OutputNar)
                .map(|c| c.median(REFERENCE_CORES as u32).round() as u64),
            own_samples: u32::try_from(own.iter().filter(|s| !s.lower_bound).count())
                .unwrap_or(u32::MAX),
            ram_floor: own
                .iter()
                .filter(|s| s.lower_bound)
                .filter_map(|s| Some((s.cores?, s.peak_ram_mb?)))
                .collect(),
            max_cores: None,
            top_features: fit(Target::Ram)
                .map(|fit| fit.top_features(&features))
                .unwrap_or_default(),
        }
    }
}

fn blend(model: LogCurve, own: &[&Sample], target: Target) -> LogCurve {
    let adjusted: Vec<f64> = own
        .iter()
        .filter(|s| !s.censored(target))
        .filter_map(|s| Some(s.value(target)? - model.per_log_core * s.log_cores(target)))
        .collect();
    if adjusted.is_empty() {
        return model;
    }
    let n = adjusted.len() as f64;
    let own_mean = adjusted.iter().sum::<f64>() / n;
    let spread = if adjusted.len() >= 3 {
        let variance = adjusted.iter().map(|v| (v - own_mean).powi(2)).sum::<f64>() / n;
        variance.sqrt().max(MIN_SPREAD)
    } else {
        model.spread
    };
    LogCurve {
        at_reference: (n * own_mean + OWN_SAMPLE_PRIOR_WEIGHT * model.at_reference)
            / (n + OWN_SAMPLE_PRIOR_WEIGHT),
        per_log_core: model.per_log_core,
        spread,
    }
}

struct Row {
    y: f64,
    l: f64,
    censored: bool,
    features: Vec<usize>,
}

/// Ridge regression in log space, solved by coordinate descent. A censored row (a lower bound)
/// only pulls the fit up, so it is active only while the prediction is below it.
struct Fitter {
    names: Vec<String>,
    rows: Vec<Row>,
    by_feature: Vec<Vec<usize>>,
    prior: Option<f64>,
    bias: f64,
    slope: f64,
    weights: Vec<f64>,
    core_weights: Vec<f64>,
    residual: Vec<f64>,
    active: Vec<bool>,
}

impl Fitter {
    fn new(samples: &[&Sample], target: Target) -> Option<Self> {
        let mut names: Vec<String> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut rows = Vec::new();
        for sample in samples {
            let Some(y) = sample.value(target) else {
                continue;
            };
            let features = sample
                .group()
                .features()
                .into_iter()
                .map(|name| {
                    *index.entry(name.clone()).or_insert_with(|| {
                        names.push(name);
                        names.len() - 1
                    })
                })
                .collect();
            rows.push(Row {
                y,
                l: sample.log_cores(target),
                censored: sample.censored(target),
                features,
            });
        }
        let measured: Vec<f64> = rows.iter().filter(|r| !r.censored).map(|r| r.y).collect();
        if measured.is_empty() {
            return None;
        }

        let mut by_feature = vec![Vec::new(); names.len()];
        for (i, row) in rows.iter().enumerate() {
            for &f in &row.features {
                by_feature[f].push(i);
            }
        }
        let prior = target.core_slope_prior();
        let bias = measured.iter().sum::<f64>() / measured.len() as f64;
        let slope = prior.unwrap_or(0.0);
        let residual = rows.iter().map(|r| r.y - bias - slope * r.l).collect();
        Some(Self {
            weights: vec![0.0; names.len()],
            core_weights: vec![0.0; names.len()],
            active: vec![true; rows.len()],
            names,
            rows,
            by_feature,
            prior,
            bias,
            slope,
            residual,
        })
    }

    fn fit(mut self) -> LinearFit {
        for _ in 0..FIT_EPOCHS {
            self.active = self
                .rows
                .iter()
                .zip(&self.residual)
                .map(|(row, e)| !row.censored || *e > 0.0)
                .collect();
            self.fit_bias();
            self.fit_slope();
            for feature in 0..self.names.len() {
                self.fit_weight(feature);
                self.fit_core_weight(feature);
            }
        }
        self.finish()
    }

    fn active_rows(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.rows.len()).filter(|&i| self.active[i])
    }

    fn shift(&mut self, rows: &[usize], by: impl Fn(&Row) -> f64) {
        for &i in rows {
            self.residual[i] -= by(&self.rows[i]);
        }
    }

    fn fit_bias(&mut self) {
        let (sum, count) = self
            .active_rows()
            .fold((0.0, 0.0), |(s, c), i| (s + self.residual[i], c + 1.0));
        if count == 0.0 {
            return;
        }
        let step = sum / count;
        self.bias += step;
        let all: Vec<usize> = (0..self.rows.len()).collect();
        self.shift(&all, |_| step);
    }

    fn fit_slope(&mut self) {
        let Some(prior) = self.prior else {
            return;
        };
        let (num, den) = self.active_rows().fold((0.0, 0.0), |(n, d), i| {
            let l = self.rows[i].l;
            (n + l * (self.residual[i] + self.slope * l), d + l * l)
        });
        let next = (num + CORE_SLOPE_PENALTY * prior) / (den + CORE_SLOPE_PENALTY);
        let step = next - self.slope;
        let all: Vec<usize> = (0..self.rows.len()).collect();
        self.shift(&all, |row| step * row.l);
        self.slope = next;
    }

    fn fit_weight(&mut self, feature: usize) {
        let members = std::mem::take(&mut self.by_feature[feature]);
        let current = self.weights[feature];
        let (num, den) = members
            .iter()
            .filter(|&&i| self.active[i])
            .fold((0.0, 0.0), |(n, d), &i| {
                (n + self.residual[i] + current, d + 1.0)
            });
        let next = num / (den + FEATURE_PENALTY);
        self.shift(&members, |_| next - current);
        self.weights[feature] = next;
        self.by_feature[feature] = members;
    }

    fn fit_core_weight(&mut self, feature: usize) {
        if self.prior.is_none() {
            return;
        }
        let members = std::mem::take(&mut self.by_feature[feature]);
        let current = self.core_weights[feature];
        let (num, den) =
            members
                .iter()
                .filter(|&&i| self.active[i])
                .fold((0.0, 0.0), |(n, d), &i| {
                    let l = self.rows[i].l;
                    (n + l * (self.residual[i] + current * l), d + l * l)
                });
        let next = num / (den + CORE_FEATURE_PENALTY);
        self.shift(&members, |row| (next - current) * row.l);
        self.core_weights[feature] = next;
        self.by_feature[feature] = members;
    }

    fn finish(self) -> LinearFit {
        let squared: Vec<f64> = self
            .rows
            .iter()
            .zip(&self.residual)
            .filter(|(row, _)| !row.censored)
            .map(|(_, e)| e * e)
            .collect();
        let error = (squared.iter().sum::<f64>() / squared.len() as f64)
            .sqrt()
            .max(MIN_SPREAD);
        let named = |values: &[f64]| {
            self.names
                .iter()
                .cloned()
                .zip(values.iter().copied())
                .filter(|(_, v)| *v != 0.0)
                .collect()
        };
        LinearFit {
            bias: self.bias,
            core_slope: self.slope,
            weights: named(&self.weights),
            core_weights: named(&self.core_weights),
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pname: &str, inputs: &[&str], cores: u32, ram: u64, ms: f64) -> Sample {
        Sample {
            derivation: DerivationId::now_v7(),
            architecture: "x86_64-linux".into(),
            pname: pname.into(),
            input_pnames: inputs.iter().map(|i| (*i).to_owned()).collect(),
            cores: Some(cores),
            peak_ram_mb: Some(ram),
            build_ms: Some(ms),
            output_nar_bytes: Some(1_000_000),
            lower_bound: false,
        }
    }

    fn key(pname: &str, inputs: &[&str]) -> GroupKey {
        sample(pname, inputs, 16, 1, 1.0).group()
    }

    fn own<'a>(samples: &'a [Sample], key: &GroupKey) -> Vec<&'a Sample> {
        samples.iter().filter(|s| &s.group() == key).collect()
    }

    fn torch_and_torch_bin() -> Vec<Sample> {
        let mut samples = Vec::new();
        for _ in 0..5 {
            samples.push(sample(
                "torch",
                &["cmake", "cudatoolkit", "magma"],
                64,
                100_000,
                3_000_000.0,
            ));
            samples.push(sample(
                "torch",
                &["python3-wheel", "autoPatchelfHook"],
                64,
                2_000,
                60_000.0,
            ));
            samples.push(sample("hello", &["gnumake"], 64, 200, 5_000.0));
        }
        samples
    }

    #[test]
    fn a_shared_pname_with_other_inputs_gets_its_own_estimate() {
        let samples = torch_and_torch_bin();
        let model = Model::fit(&samples);
        let source = key("torch", &["cmake", "cudatoolkit", "magma"]);
        let wheel = key("torch", &["python3-wheel", "autoPatchelfHook"]);

        let source_ram = model
            .estimate(&source, &own(&samples, &source))
            .ram_mb(64)
            .unwrap();
        let wheel_ram = model
            .estimate(&wheel, &own(&samples, &wheel))
            .ram_mb(64)
            .unwrap();

        assert!(source_ram > 50_000, "source build {source_ram}");
        assert!(wheel_ram < 10_000, "wheel build {wheel_ram}");
    }

    #[test]
    fn a_build_never_seen_is_estimated_from_its_inputs() {
        let samples = torch_and_torch_bin();
        let model = Model::fit(&samples);
        let bumped = key("torch", &["cmake", "cudatoolkit", "magma", "nccl"]);
        let unknown = key("never-built", &["also-unknown"]);

        let bumped = model.estimate(&bumped, &[]);
        let unknown = model.estimate(&unknown, &[]);

        assert_eq!(bumped.own_samples, 0);
        assert!(bumped.ram_mb(64).unwrap() > 20_000, "{bumped:?}");
        assert!(unknown.ram_mb(16).unwrap() > 0 && unknown.build_ms(16).unwrap() > 0.0);
    }

    #[test]
    fn one_own_build_moves_the_estimate_only_part_of_the_way() {
        let mut samples: Vec<Sample> = (0..10)
            .map(|_| sample("lib", &["cmake"], 16, 1_000, 10_000.0))
            .collect();
        samples.push(sample("lib", &["cmake", "extra"], 16, 8_000, 10_000.0));
        let model = Model::fit(&samples[..10]);
        let changed = key("lib", &["cmake", "extra"]);

        let median = model
            .estimate(&changed, &own(&samples, &changed))
            .ram
            .unwrap()
            .median(16);

        assert!(median > 1_000.0 && median < 8_000.0, "{median}");
    }

    #[test]
    fn an_out_of_memory_kill_raises_the_ram_at_its_core_count_only() {
        let mut samples = torch_and_torch_bin();
        let mut killed = sample("big", &["cmake"], 64, 117_905, 1.0);
        killed.lower_bound = true;
        killed.build_ms = None;
        samples.push(killed);
        let model = Model::fit(&samples);
        let big = key("big", &["cmake"]);
        let estimate = model.estimate(&big, &own(&samples, &big));

        assert!(estimate.ram_mb(64).unwrap() > 117_905, "{estimate:?}");
        assert!(estimate.ram_mb(43).unwrap() < estimate.ram_mb(64).unwrap());
    }

    #[test]
    fn ram_follows_the_core_count_the_samples_show() {
        let mut samples = Vec::new();
        for _ in 0..6 {
            samples.push(sample("par", &["cc"], 8, 4_000, 20_000.0));
            samples.push(sample("par", &["cc"], 32, 16_000, 6_000.0));
        }
        let model = Model::fit(&samples);
        let par = key("par", &["cc"]);
        let estimate = model.estimate(&par, &own(&samples, &par));

        assert!(
            estimate.ram_mb(32).unwrap() > 2 * estimate.ram_mb(8).unwrap(),
            "{estimate:?}"
        );
        assert!(estimate.build_ms(32).unwrap() < estimate.build_ms(8).unwrap());
    }

    #[test]
    fn an_architecture_without_samples_has_no_estimate() {
        let model = Model::fit(&torch_and_torch_bin());
        let mut other = key("torch", &["cmake"]);
        other.architecture = "aarch64-linux".into();

        let estimate = model.estimate(&other, &[]);

        assert_eq!((estimate.ram, estimate.build_time), (None, None));
        assert!(Model::fit(&[]).estimate(&other, &[]).ram.is_none());
    }
}
