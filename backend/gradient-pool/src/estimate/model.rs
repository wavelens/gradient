/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use gradient_types::ids::DerivationId;

use super::{BuildEstimate, LogCurve, log_cores};
use crate::score::weights::{
    BUILD_TIME_CORE_SLOPE, FEW_SAMPLES_SPREAD, MIN_INPUT_OVERLAP, MIN_SPREAD,
    OWN_SAMPLE_PRIOR_WEIGHT, RAM_CORE_SLOPE, REFERENCE_CORES, SIMILAR_BUILDS,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Target {
    Ram,
    BuildTime,
    OutputNar,
}

impl Target {
    const ALL: [Self; 3] = [Self::Ram, Self::BuildTime, Self::OutputNar];

    const fn core_slope(self) -> f64 {
        match self {
            Self::Ram => RAM_CORE_SLOPE,
            Self::BuildTime => BUILD_TIME_CORE_SLOPE,
            Self::OutputNar => 0.0,
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

pub fn input_overlap(a: &[String], b: &[String]) -> f64 {
    let (a, b): (HashSet<&String>, HashSet<&String>) = (a.iter().collect(), b.iter().collect());
    let union = a.union(&b).count();
    if union == 0 {
        return 1.0;
    }
    a.intersection(&b).count() as f64 / union as f64
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

    fn at_reference(&self, target: Target) -> Option<f64> {
        let cores = self.cores.map_or(0.0, log_cores);
        Some(self.value(target)? - target.core_slope() * cores)
    }
}

/// The newest measured build of every pname on one architecture, and the pnames per input.
#[derive(Clone, Debug, Default)]
struct Builds {
    newest: HashMap<String, Sample>,
    by_input: HashMap<String, Vec<String>>,
    overall: HashMap<Target, LogCurve>,
}

impl Builds {
    fn new(samples: Vec<&Sample>) -> Self {
        let mut newest = HashMap::new();
        for sample in samples.into_iter().filter(|s| !s.lower_bound) {
            newest.insert(sample.pname.clone(), sample.clone());
        }
        let mut by_input: HashMap<String, Vec<String>> = HashMap::new();
        for sample in newest.values() {
            for input in &sample.input_pnames {
                by_input
                    .entry(input.clone())
                    .or_default()
                    .push(sample.pname.clone());
            }
        }
        let overall = Target::ALL
            .into_iter()
            .filter_map(|target| {
                let mut values: Vec<f64> = newest
                    .values()
                    .filter_map(|s| s.at_reference(target))
                    .collect();
                Some((target, median_curve(&mut values, target)?))
            })
            .collect();
        Self {
            newest,
            by_input,
            overall,
        }
    }

    fn similar(&self, key: &GroupKey) -> Vec<(f64, &Sample)> {
        let mut shared: HashMap<&str, usize> = HashMap::new();
        let inputs: HashSet<&String> = key.input_pnames.iter().collect();
        for input in &inputs {
            for pname in self.by_input.get(*input).into_iter().flatten() {
                *shared.entry(pname).or_default() += 1;
            }
        }
        let mut similar: Vec<(f64, &Sample)> = shared
            .into_iter()
            .map(|(pname, both)| {
                let sample = &self.newest[pname];
                let union = inputs.len() + sample.input_pnames.len() - both;
                (both as f64 / union as f64, sample)
            })
            .filter(|(overlap, _)| *overlap >= MIN_INPUT_OVERLAP)
            .collect();
        similar.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.pname.cmp(&b.1.pname)));
        similar.truncate(SIMILAR_BUILDS);
        similar
    }

    fn curve(&self, similar: &[(f64, &Sample)], target: Target) -> Option<LogCurve> {
        let points: Vec<(f64, f64)> = similar
            .iter()
            .filter_map(|(overlap, s)| Some((overlap * overlap, s.at_reference(target)?)))
            .collect();
        let total: f64 = points.iter().map(|(weight, _)| weight).sum();
        if total <= 0.0 {
            return self.overall.get(&target).copied();
        }
        let mean = points.iter().map(|(w, v)| w * v).sum::<f64>() / total;
        let variance = points
            .iter()
            .map(|(w, v)| w * (v - mean).powi(2))
            .sum::<f64>()
            / total;
        Some(LogCurve {
            at_reference: mean,
            per_log_core: target.core_slope(),
            spread: variance.sqrt().max(FEW_SAMPLES_SPREAD),
        })
    }
}

fn median_curve(values: &mut [f64], target: Target) -> Option<LogCurve> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let median = values[values.len() / 2];
    let variance = values.iter().map(|v| (v - median).powi(2)).sum::<f64>() / values.len() as f64;
    Some(LogCurve {
        at_reference: median,
        per_log_core: target.core_slope(),
        spread: variance.sqrt().max(FEW_SAMPLES_SPREAD),
    })
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    architectures: HashMap<String, Builds>,
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
        Self {
            architectures: by_architecture
                .into_iter()
                .map(|(architecture, samples)| (architecture.to_owned(), Builds::new(samples)))
                .collect(),
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded and positive"
    )]
    pub fn estimate(&self, key: &GroupKey, own: &[(f64, &Sample)]) -> BuildEstimate {
        let Some(builds) = self.architectures.get(&key.architecture) else {
            return BuildEstimate::default();
        };
        let similar = builds.similar(key);
        let curve = |target: Target| Some(blend(builds.curve(&similar, target)?, own, target));
        BuildEstimate {
            ram: curve(Target::Ram),
            build_time: curve(Target::BuildTime),
            output_nar_bytes: curve(Target::OutputNar)
                .map(|c| c.median(REFERENCE_CORES).round() as u64),
            own_samples: u32::try_from(own.iter().filter(|(_, s)| !s.lower_bound).count())
                .unwrap_or(u32::MAX),
            ram_floor: own
                .iter()
                .filter(|(_, s)| s.lower_bound)
                .filter_map(|(_, s)| Some((s.cores?, s.peak_ram_mb?)))
                .collect(),
            max_cores: None,
            similar_builds: similar
                .iter()
                .map(|(overlap, s)| (s.pname.clone(), *overlap))
                .collect(),
        }
    }
}

/// Own builds count at the cores they ran with. The core term only shapes the estimate from
/// similar builds, so it fades as own builds add up.
fn blend(model: LogCurve, own: &[(f64, &Sample)], target: Target) -> LogCurve {
    let measured: Vec<(f64, f64)> = own
        .iter()
        .filter(|(_, s)| !s.censored(target))
        .filter_map(|(weight, s)| Some((*weight, s.value(target)?)))
        .collect();
    let total: f64 = measured.iter().map(|(weight, _)| weight).sum();
    if total <= 0.0 {
        return model;
    }
    let own_mean = measured.iter().map(|(w, v)| w * v).sum::<f64>() / total;
    let spread = if total >= 3.0 {
        let variance = measured
            .iter()
            .map(|(w, v)| w * (v - own_mean).powi(2))
            .sum::<f64>()
            / total;
        variance.sqrt().max(MIN_SPREAD)
    } else {
        FEW_SAMPLES_SPREAD
    };
    let model_share = OWN_SAMPLE_PRIOR_WEIGHT / (total + OWN_SAMPLE_PRIOR_WEIGHT);
    LogCurve {
        at_reference: own_mean + model_share * (model.at_reference - own_mean),
        per_log_core: model_share * model.per_log_core,
        spread,
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

    fn own<'a>(samples: &'a [Sample], key: &GroupKey) -> Vec<(f64, &'a Sample)> {
        samples
            .iter()
            .filter(|s| &s.group() == key)
            .map(|s| (1.0, s))
            .collect()
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

    fn python_packages() -> Vec<Sample> {
        vec![
            sample(
                "llama-index-llms-openai",
                &["python3", "hatchling", "llama-index-core", "openai"],
                16,
                126,
                7_000.0,
            ),
            sample(
                "cuda-kernels",
                &["cmake", "cudatoolkit", "ninja"],
                16,
                50_000,
                900_000.0,
            ),
            sample("unit-file", &["bash"], 16, 5, 100.0),
        ]
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
    fn a_build_never_seen_takes_the_builds_with_the_most_similar_inputs() {
        let model = Model::fit(&python_packages());
        let unseen = key(
            "llama-index-embeddings-openai",
            &[
                "python3",
                "hatchling",
                "llama-index-core",
                "openai",
                "numpy",
            ],
        );

        let estimate = model.estimate(&unseen, &[]);

        let median = estimate.ram.unwrap().median(16);
        assert!(median > 60.0 && median < 250.0, "{estimate:?}");
        assert_eq!(estimate.similar_builds[0].0, "llama-index-llms-openai");
        assert_eq!(estimate.own_samples, 0);
    }

    #[test]
    fn a_build_without_similar_inputs_gets_the_median_of_all_builds() {
        let model = Model::fit(&python_packages());

        let estimate = model.estimate(&key("never-built", &["also-unknown"]), &[]);

        let median = estimate.ram.unwrap().median(16);
        assert!((median - 126.0).abs() < 1.0, "{estimate:?}");
        assert!(estimate.similar_builds.is_empty());
        assert!(estimate.build_ms(16).unwrap() > 0.0);
    }

    #[test]
    fn an_out_of_memory_kill_is_no_similar_build() {
        let mut samples = python_packages();
        let mut killed = sample(
            "llama-index-core",
            &["python3", "hatchling", "openai"],
            16,
            90_000,
            1.0,
        );
        killed.lower_bound = true;
        killed.build_ms = None;
        samples.push(killed);
        let model = Model::fit(&samples);

        let estimate = model.estimate(&key("other", &["python3", "hatchling", "openai"]), &[]);

        assert!(estimate.ram_mb(16).unwrap() < 1_000, "{estimate:?}");
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
    fn own_builds_count_at_the_cores_they_ran_with() {
        let mut samples = torch_and_torch_bin();
        for _ in 0..3 {
            samples.push(sample("flat", &["cc"], 16, 4_000, 20_000.0));
        }
        let model = Model::fit(&samples);
        let flat = key("flat", &["cc"]);
        let estimate = model.estimate(&flat, &own(&samples, &flat));

        let (at_16, at_64) = (estimate.ram_mb(16).unwrap(), estimate.ram_mb(64).unwrap());
        assert!(
            at_64 * 10 < at_16 * 12,
            "{at_16} at 16 cores, {at_64} at 64"
        );
    }

    #[test]
    fn a_similar_build_counts_at_the_cores_it_ran_with() {
        let model = Model::fit(&[sample("par", &["cc", "make"], 64, 16_000, 6_000.0)]);

        let estimate = model.estimate(&key("other", &["cc", "make"]), &[]);

        assert!(estimate.ram.unwrap().median(16) < 16_000.0, "{estimate:?}");
        assert!(estimate.ram_mb(64).unwrap() > estimate.ram_mb(16).unwrap());
        assert!(estimate.build_ms(64).unwrap() < estimate.build_ms(16).unwrap());
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
