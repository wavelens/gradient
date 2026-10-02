/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A fixture directory is holding an `output` file and a `store/` directory of ATerm `.drv` files.
//! `output` is containing the entry-point `.drv` path on one line. A BFS from the entry point
//! through `inputDrvs` is building the full closure.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use gradient_derivation::{Derivation, parse_drv};
use gradient_wire::messages::{DerivationOutput, DiscoveredDerivation};

use super::derivation_resolver::FakeDerivationResolver;
use super::nix_store::FakeNixStoreProvider;

pub struct StoreFixture {
    pub entry_point: String,
    pub derivations: Vec<DiscoveredDerivation>,
    pub tree: HashMap<String, Vec<String>>,
    pub parsed: HashMap<String, Derivation>,
    pub raw_drvs: HashMap<String, Vec<u8>>,
    pub resolver: FakeDerivationResolver,
    pub store: FakeNixStoreProvider,
}

pub fn load_store(dir: &Path) -> StoreFixture {
    let output_path = dir.join("output");
    let entry_point = std::fs::read_to_string(&output_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", output_path.display(), e))
        .trim()
        .to_string();

    let store_dir = dir.join("store");
    let mut parsed: HashMap<String, Derivation> = HashMap::new();
    let mut raw_drvs: HashMap<String, Vec<u8>> = HashMap::new();

    for entry in std::fs::read_dir(&store_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", store_dir.display(), e))
    {
        let entry = entry.expect("failed to read dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("drv") {
            continue;
        }
        let filename = path.file_name().unwrap().to_str().unwrap();
        let store_path = format!("/nix/store/{}", filename);
        let content = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));
        let drv = parse_drv(&content)
            .unwrap_or_else(|e| panic!("failed to parse {}: {}", path.display(), e));
        raw_drvs.insert(store_path.clone(), content);
        parsed.insert(store_path, drv);
    }

    let mut derivations: Vec<DiscoveredDerivation> = Vec::new();
    let mut tree: HashMap<String, Vec<String>> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    if visited.insert(entry_point.clone()) {
        queue.push_back(entry_point.clone());
    }

    while let Some(drv_path) = queue.pop_front() {
        let drv = match parsed.get(&drv_path) {
            Some(d) => d,
            None => panic!(
                "BFS reached {} but no .drv file found in store/ directory",
                drv_path
            ),
        };

        let dep_paths: Vec<String> = drv
            .input_derivations
            .iter()
            .map(|(p, _)| p.clone())
            .collect();

        for dep in &dep_paths {
            if visited.insert(dep.clone()) {
                queue.push_back(dep.clone());
            }
        }

        tree.insert(drv_path.clone(), dep_paths.clone());

        let outputs: Vec<DerivationOutput> = drv
            .outputs
            .iter()
            .filter(|o| !o.path.is_empty())
            .map(|o| DerivationOutput {
                name: o.name.clone(),
                path: o.path.clone(),
            })
            .collect();

        derivations.push(DiscoveredDerivation {
            attr: String::new(),
            drv_path: drv_path.clone(),
            outputs,
            dependencies: dep_paths,
            input_sources: drv.input_sources.clone(),
            architecture: drv.system.clone(),
            required_features: drv.required_system_features(),
            timeout_secs: None,
            max_silent_secs: None,
            prefer_local_build: false,
            is_fixed_output: drv.build_meta().is_fixed_output,
            allow_substitutes: drv.allow_substitutes(),
            pname: gradient_derivation::derive_pname(
                drv.environment.get("pname").map(String::as_str),
                drv.environment
                    .get("name")
                    .map(String::as_str)
                    .unwrap_or(""),
            ),
        });
    }

    let mut resolver = FakeDerivationResolver::new();
    for (store_path, drv) in &parsed {
        resolver = resolver.with_derivation(store_path.clone(), drv.clone());
        let (arch, feats) = (drv.system.clone(), drv.required_system_features());
        resolver = resolver.with_features(store_path.clone(), arch, feats);
    }

    let store = FakeNixStoreProvider::new();

    StoreFixture {
        entry_point,
        derivations,
        tree,
        parsed,
        raw_drvs,
        resolver,
        store,
    }
}

impl StoreFixture {
    pub fn mark_built(&mut self, drv_path: &str) {
        let drv = self
            .derivations
            .iter()
            .find(|d| d.drv_path == drv_path)
            .unwrap_or_else(|| panic!("mark_built: unknown drv_path {}", drv_path));
        for output in &drv.outputs {
            self.store = std::mem::take(&mut self.store).with_present_path(output.path.clone());
        }
    }

    pub fn mark_subtree_built(&mut self, drv_path: &str) {
        let mut stack = vec![drv_path.to_string()];
        let mut visited = HashSet::new();
        while let Some(path) = stack.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            self.mark_built(&path);
            if let Some(deps) = self.tree.get(&path) {
                stack.extend(deps.iter().cloned());
            }
        }
    }

    pub fn mark_all_built(&mut self) {
        let paths: Vec<String> = self
            .derivations
            .iter()
            .map(|d| d.drv_path.clone())
            .collect();
        for path in paths {
            self.mark_built(&path);
        }
    }

    pub fn remove_random_subtrees(&mut self, fraction: f64, seed: u64) {
        let mut rng_state = seed;
        let drv_paths: Vec<String> = self
            .derivations
            .iter()
            .map(|d| d.drv_path.clone())
            .collect();

        for drv_path in &drv_paths {
            rng_state = rng_state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let rand_val = (rng_state >> 33) as f64 / (1u64 << 31) as f64;

            if rand_val < fraction {
                let drv = self
                    .derivations
                    .iter()
                    .find(|d| d.drv_path == *drv_path)
                    .unwrap();
                for output in &drv.outputs {
                    self.store.remove_present_path(&output.path);
                }
            }
        }
    }

    pub fn unbuilt(&self) -> Vec<&DiscoveredDerivation> {
        self.derivations
            .iter()
            .filter(|d| !self.is_built(d))
            .collect()
    }

    pub fn built(&self) -> Vec<&DiscoveredDerivation> {
        self.derivations
            .iter()
            .filter(|d| self.is_built(d))
            .collect()
    }

    pub fn startable(&self) -> Vec<&DiscoveredDerivation> {
        self.derivations
            .iter()
            .filter(|d| {
                if self.is_built(d) {
                    return false;
                }
                d.dependencies.iter().all(|dep_path| {
                    self.derivations
                        .iter()
                        .find(|dd| dd.drv_path == *dep_path)
                        .map(|dd| self.is_built(dd))
                        .unwrap_or(true)
                })
            })
            .collect()
    }

    fn is_built(&self, drv: &DiscoveredDerivation) -> bool {
        if drv.outputs.is_empty() {
            return false;
        }
        let present = self.store.present_paths();
        drv.outputs.iter().all(|o| present.contains(&o.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test-store")
    }

    #[test]
    fn load_store_parses_all_derivations() {
        let fixture = load_store(&fixture_dir());
        let expected_entry_point = std::fs::read_to_string(fixture_dir().join("output"))
            .expect("read test-store/output")
            .trim()
            .to_string();
        assert_eq!(fixture.entry_point, expected_entry_point);
        assert!(fixture.entry_point.starts_with("/nix/store/"));
        assert!(fixture.entry_point.ends_with(".drv"));
        assert!(!fixture.derivations.is_empty());
        assert!(
            fixture
                .derivations
                .iter()
                .any(|d| d.drv_path == fixture.entry_point)
        );
    }

    #[test]
    fn tree_has_entry_for_every_derivation() {
        let fixture = load_store(&fixture_dir());
        for drv in &fixture.derivations {
            assert!(
                fixture.tree.contains_key(&drv.drv_path),
                "tree missing entry for {}",
                drv.drv_path
            );
        }
    }

    #[test]
    fn initially_nothing_is_built() {
        let fixture = load_store(&fixture_dir());
        assert!(fixture.built().is_empty());
        assert_eq!(fixture.unbuilt().len(), fixture.derivations.len());
    }

    #[test]
    fn mark_all_built_then_everything_is_built() {
        let mut fixture = load_store(&fixture_dir());
        fixture.mark_all_built();
        assert_eq!(fixture.built().len(), fixture.derivations.len());
        assert!(fixture.unbuilt().is_empty());
        assert!(fixture.startable().is_empty());
    }

    #[test]
    fn leaf_nodes_can_start() {
        let fixture = load_store(&fixture_dir());
        let startable = fixture.startable();
        for drv in &startable {
            assert!(
                drv.dependencies.is_empty(),
                "{} can start but has dependencies: {:?}",
                drv.drv_path,
                drv.dependencies
            );
        }
        assert!(
            !startable.is_empty(),
            "there should be at least one leaf node"
        );
    }

    #[test]
    fn mark_subtree_built_includes_transitive_deps() {
        let mut fixture = load_store(&fixture_dir());
        fixture.mark_subtree_built(&fixture.entry_point.clone());
        assert_eq!(fixture.built().len(), fixture.derivations.len());
    }

    #[test]
    fn remove_random_subtrees_creates_partial_store() {
        let mut fixture = load_store(&fixture_dir());
        fixture.mark_all_built();
        fixture.remove_random_subtrees(0.5, 42);
        let built = fixture.built().len();
        let unbuilt = fixture.unbuilt().len();
        assert!(built > 0, "some should remain built");
        assert!(unbuilt > 0, "some should be unbuilt");
        assert_eq!(built + unbuilt, fixture.derivations.len());
    }

    #[test]
    fn startable_converges() {
        let mut fixture = load_store(&fixture_dir());
        let total = fixture.derivations.len();
        let mut waves = 0;

        loop {
            let startable = fixture.startable();
            if startable.is_empty() {
                break;
            }
            let paths: Vec<String> = startable.iter().map(|d| d.drv_path.clone()).collect();
            for path in &paths {
                fixture.mark_built(path);
            }
            waves += 1;
            assert!(
                waves <= total,
                "convergence loop exceeded total derivations"
            );
        }

        assert_eq!(fixture.built().len(), total);
        assert!(waves > 1, "should take more than 1 wave for a real fixture");
    }

    #[test]
    fn remove_random_subtrees_is_deterministic() {
        let mut fixture1 = load_store(&fixture_dir());
        fixture1.mark_all_built();
        fixture1.remove_random_subtrees(0.3, 12345);

        let mut fixture2 = load_store(&fixture_dir());
        fixture2.mark_all_built();
        fixture2.remove_random_subtrees(0.3, 12345);

        let built1: HashSet<String> = fixture1
            .built()
            .iter()
            .map(|d| d.drv_path.clone())
            .collect();
        let built2: HashSet<String> = fixture2
            .built()
            .iter()
            .map(|d| d.drv_path.clone())
            .collect();
        assert_eq!(built1, built2);
    }

    #[test]
    fn single_leaf_unbuilt() {
        let mut fixture = load_store(&fixture_dir());
        fixture.mark_all_built();

        let leaf = fixture
            .derivations
            .iter()
            .find(|d| d.dependencies.is_empty())
            .unwrap()
            .drv_path
            .clone();

        for output in &fixture
            .derivations
            .iter()
            .find(|d| d.drv_path == leaf)
            .unwrap()
            .outputs
            .clone()
        {
            fixture.store.remove_present_path(&output.path);
        }

        let startable = fixture.startable();
        assert!(
            startable.iter().any(|d| d.drv_path == leaf),
            "unbuilt leaf can start"
        );
    }

    #[test]
    fn startable_respects_dependencies() {
        let mut fixture = load_store(&fixture_dir());
        let leaves: Vec<String> = fixture
            .derivations
            .iter()
            .filter(|d| d.dependencies.is_empty())
            .map(|d| d.drv_path.clone())
            .collect();
        for leaf in &leaves {
            fixture.mark_built(leaf);
        }
        let startable = fixture.startable();
        for drv in &startable {
            assert!(!fixture.is_built(drv));
            for dep in &drv.dependencies {
                let dep_drv = fixture
                    .derivations
                    .iter()
                    .find(|d| d.drv_path == *dep)
                    .unwrap();
                assert!(
                    fixture.is_built(dep_drv),
                    "{} can start but dep {} is not built",
                    drv.drv_path,
                    dep
                );
            }
        }
    }
}
