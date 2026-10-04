/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::UserId;
use rand::distr::{Alphanumeric, SampleString};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Dir {
    user: UserId,
    created: Instant,
    links: HashMap<PathBuf, String>,
}

pub struct Roots {
    ttl: Duration,
    dirs: Mutex<HashMap<PathBuf, Dir>>,
}

impl Roots {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            dirs: Mutex::new(HashMap::new()),
        }
    }

    pub fn make_dir(&self, user: UserId, pattern: &str) -> PathBuf {
        let random = Alphanumeric.sample_string(&mut rand::rng(), pattern.len());
        let name: String = pattern
            .chars()
            .zip(random.chars())
            .map(|(p, r)| match p {
                'X' => r,
                '/' => '_',
                other => other,
            })
            .collect();
        let name = if name.chars().all(|c| c == '.') {
            random
        } else {
            name
        };

        let dir = Path::new("/tmp").join(name);
        self.with_dirs(|dirs| {
            dirs.insert(
                dir.clone(),
                Dir {
                    user,
                    created: Instant::now(),
                    links: HashMap::new(),
                },
            );
        });
        dir
    }

    pub fn add(&self, user: UserId, root: &Path, target: String) -> bool {
        let Some(parent) = root.parent() else {
            return false;
        };

        self.with_dirs(|dirs| match dirs.get_mut(parent) {
            Some(dir) if dir.user == user => {
                dir.links.insert(root.to_path_buf(), target);
                true
            }
            _ => false,
        })
    }

    pub fn resolve(&self, user: UserId, path: &Path) -> Option<String> {
        let parent = path.parent()?;
        self.with_dirs(|dirs| {
            dirs.get(parent)
                .filter(|dir| dir.user == user)
                .and_then(|dir| dir.links.get(path).cloned())
        })
    }

    pub fn remove_dir(&self, user: UserId, dir: &Path) {
        self.with_dirs(|dirs| {
            if dirs.get(dir).is_some_and(|d| d.user == user) {
                dirs.remove(dir);
            }
        });
    }

    fn with_dirs<T>(&self, f: impl FnOnce(&mut HashMap<PathBuf, Dir>) -> T) -> T {
        let mut dirs = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        let ttl = self.ttl;
        dirs.retain(|_, dir| dir.created.elapsed() < ttl);
        f(&mut dirs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_in_a_made_dir_resolves() {
        let roots = Roots::new(Duration::from_secs(3600));
        let user = UserId::now_v7();
        let dir = roots.make_dir(user, "nixos-rebuild.XXXXX");
        let suffix = dir
            .to_string_lossy()
            .strip_prefix("/tmp/nixos-rebuild.")
            .map(str::to_owned)
            .expect("the pattern prefix is kept");
        assert_eq!(suffix.len(), 5);
        assert!(suffix.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(suffix, "XXXXX");

        let root = dir.join("f00");
        assert!(roots.add(user, &root, "/nix/store/aaa-system".into()));
        assert_eq!(
            roots.resolve(user, &root).as_deref(),
            Some("/nix/store/aaa-system")
        );
    }

    #[test]
    fn another_user_cannot_resolve_or_add() {
        let roots = Roots::new(Duration::from_secs(3600));
        let owner = UserId::now_v7();
        let dir = roots.make_dir(owner, "x.XXXXX");
        roots.add(owner, &dir.join("r"), "/nix/store/aaa-x".into());

        let other = UserId::now_v7();
        assert!(roots.resolve(other, &dir.join("r")).is_none());
        assert!(!roots.add(other, &dir.join("s"), "/nix/store/bbb-y".into()));
    }

    #[test]
    fn removing_the_dir_drops_its_roots() {
        let roots = Roots::new(Duration::from_secs(3600));
        let user = UserId::now_v7();
        let dir = roots.make_dir(user, "x.XXXXX");
        roots.add(user, &dir.join("r"), "/nix/store/aaa-x".into());
        roots.remove_dir(user, &dir);
        assert!(roots.resolve(user, &dir.join("r")).is_none());
    }

    #[test]
    fn expired_dirs_are_gone() {
        let roots = Roots::new(Duration::ZERO);
        let user = UserId::now_v7();
        let dir = roots.make_dir(user, "x.XXXXX");
        assert!(!roots.add(user, &dir.join("r"), "/nix/store/aaa-x".into()));
    }

    #[test]
    fn a_pattern_cannot_leave_tmp() {
        let roots = Roots::new(Duration::from_secs(3600));
        let dir = roots.make_dir(UserId::now_v7(), "../etc/XXXX");
        assert_eq!(dir.parent(), Some(std::path::Path::new("/tmp")));
    }
}
