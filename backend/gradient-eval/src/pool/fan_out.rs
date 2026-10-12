/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::VecDeque;
use std::future::Future;

use futures::stream::{FuturesUnordered, StreamExt};

use crate::ipc::{AttrError, DiscoveryShard, ResolvedItem};
use crate::stats::StatsDelta;

use super::transport::Listing;

pub const MAX_CRASH_ATTEMPTS: u32 = 2;

const MAX_BATCH: usize = 64;

fn batch_size(items: usize, workers: usize) -> usize {
    items.div_ceil(workers.max(1) * 4).clamp(1, MAX_BATCH)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiscoveryCall {
    pub(super) wildcards: Vec<String>,
    pub(super) only: Option<Vec<String>>,
}

pub(super) fn discovery_calls(
    shards: Vec<DiscoveryShard>,
    excludes: &[String],
    workers: usize,
) -> Vec<DiscoveryCall> {
    let wildcards = |pattern: String| {
        let mut w = Vec::with_capacity(1 + excludes.len());
        w.push(pattern);
        w.extend_from_slice(excludes);
        w
    };

    shards
        .into_iter()
        .flat_map(|shard| match shard.only {
            None => vec![DiscoveryCall {
                wildcards: wildcards(shard.pattern),
                only: None,
            }],
            Some(names) => names
                .chunks(batch_size(names.len(), workers))
                .map(|chunk| DiscoveryCall {
                    wildcards: wildcards(shard.pattern.clone()),
                    only: Some(chunk.to_vec()),
                })
                .collect(),
        })
        .collect()
}

fn segment_matches(pattern: &str, attr: &str) -> bool {
    pattern == "*" || pattern == attr
}

pub(super) fn entry_point_of(attr: &str, patterns: &[String]) -> String {
    let attr_segs: Vec<&str> = attr.split('.').collect();
    let matches = |pat: &str| {
        let pat_segs: Vec<&str> = pat.split('.').collect();
        pat_segs.len() <= attr_segs.len()
            && pat_segs
                .iter()
                .zip(&attr_segs)
                .all(|(p, a)| segment_matches(p, a))
    };

    patterns
        .iter()
        .filter(|p| matches(p))
        .max_by_key(|p| {
            let segs = p.split('.').count();
            let literals = p.split('.').filter(|s| !s.contains('*')).count();
            (segs, literals)
        })
        .cloned()
        .unwrap_or_else(|| attr_segs.first().copied().unwrap_or("").to_string())
}

/// Split crashed batches until single members remain, then retry a lone member or whole attribute
/// once before failing only that attribute.
pub(super) fn crashed_listing(
    call: &DiscoveryCall,
    attempt: u32,
    crash: &anyhow::Error,
) -> Option<Listing> {
    let pattern = call.wildcards.first()?;
    let mut listing = Listing::default();
    let attr = match call.only.as_deref() {
        Some([]) => return None,
        Some(names @ [_, _, ..]) => {
            listing.deferred.push(DiscoveryShard {
                pattern: pattern.clone(),
                only: Some(names.to_vec()),
            });
            return Some(listing);
        }
        _ if attempt < MAX_CRASH_ATTEMPTS => return None,
        Some([name]) => member_attr(pattern, name),
        None => pattern.clone(),
    };
    listing.errors.push(AttrError {
        attr,
        message: format!("evaluator crashed while listing this attribute: {crash:#}"),
    });
    Some(listing)
}

fn member_attr(pattern: &str, name: &str) -> String {
    match pattern.rsplit_once('.') {
        Some((prefix, _)) => format!("{prefix}.{name}"),
        None => name.to_string(),
    }
}

pub(super) async fn pooled_fan_out<T, Fut>(
    workers: usize,
    items: Vec<T>,
    run: impl Fn(T) -> Fut,
) -> anyhow::Result<()>
where
    Fut: Future<Output = anyhow::Result<Vec<T>>>,
{
    let mut queue = VecDeque::from(items);
    let mut running = FuturesUnordered::new();
    loop {
        while running.len() < workers.max(1)
            && let Some(item) = queue.pop_front()
        {
            running.push(run(item));
        }

        let Some(done) = running.next().await else {
            return Ok(());
        };
        queue.extend(done?);
    }
}

pub type FoundItems<'a> = &'a (dyn Fn(Vec<ResolvedItem>) + Send + Sync);

/// A completed `List` call, reported so the caller can account for it.
pub struct CallStats {
    pub entry_point: String,
    pub stats: Option<StatsDelta>,
    pub rss: u64,
}

pub struct Discovery {
    pub warnings: Vec<String>,
    pub errors: Vec<AttrError>,
    pub calls: Vec<CallStats>,
}

impl Discovery {
    pub(super) fn new(
        warnings: Vec<String>,
        errors: Vec<AttrError>,
        calls: Vec<CallStats>,
    ) -> Self {
        Self {
            warnings,
            errors,
            calls,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use super::super::lock;

    #[test]
    fn a_restricted_shard_is_listed_in_batches_carrying_the_exclusions() {
        let names: Vec<String> = (0..10).map(|i| format!("host{i}")).collect();
        let shards = vec![
            DiscoveryShard {
                pattern: "hydraJobs.*".into(),
                only: Some(names.clone()),
            },
            DiscoveryShard {
                pattern: "packages.x86_64-linux.hello".into(),
                only: None,
            },
        ];
        let excludes = vec!["!hydraJobs.host3".to_string()];

        let calls = discovery_calls(shards, &excludes, 1);

        let batched: Vec<&DiscoveryCall> = calls.iter().filter(|c| c.only.is_some()).collect();
        assert_eq!(batched.len(), 4, "10 names over 1 worker: batches of 3");
        let listed: Vec<String> = batched
            .iter()
            .flat_map(|c| c.only.clone().unwrap_or_default())
            .collect();
        assert_eq!(listed, names);
        assert!(calls.iter().all(|c| c.wildcards.ends_with(&excludes)));
        assert!(calls.contains(&DiscoveryCall {
            wildcards: vec![
                "packages.x86_64-linux.hello".into(),
                "!hydraJobs.host3".into()
            ],
            only: None,
        }));
    }

    #[test]
    fn a_crashed_batch_is_split_and_a_lone_member_or_whole_attribute_fails_after_a_retry() {
        let crash = anyhow::anyhow!("eval worker closed pipe: signal: 9 (SIGKILL)");
        let batch = DiscoveryCall {
            wildcards: vec!["hydraJobs.*".into(), "!hydraJobs.ap1".into()],
            only: Some(vec!["ap10".into(), "ap11".into()]),
        };
        let lone = DiscoveryCall {
            wildcards: vec!["hydraJobs.*".into()],
            only: Some(vec!["ap10".into()]),
        };
        let whole = DiscoveryCall {
            wildcards: vec![
                "nixosConfigurations.host.config.system.build.toplevel".into(),
                "!hydraJobs.ap1".into(),
            ],
            only: None,
        };

        let split = crashed_listing(&batch, 1, &crash).expect("a batch splits at once");
        assert_eq!(
            split.deferred,
            vec![DiscoveryShard {
                pattern: "hydraJobs.*".into(),
                only: Some(vec!["ap10".into(), "ap11".into()]),
            }]
        );
        assert!(split.errors.is_empty());

        assert!(crashed_listing(&lone, 1, &crash).is_none(), "retried once");
        let failed = crashed_listing(&lone, MAX_CRASH_ATTEMPTS, &crash).expect("then fails");
        assert_eq!(failed.errors.len(), 1);
        assert_eq!(failed.errors[0].attr, "hydraJobs.ap10");
        assert!(failed.errors[0].message.contains("SIGKILL"), "{failed:?}");

        assert!(crashed_listing(&whole, 1, &crash).is_none(), "retried once");
        let failed = crashed_listing(&whole, MAX_CRASH_ATTEMPTS, &crash).expect("then fails");
        assert_eq!(failed.errors.len(), 1);
        assert_eq!(
            failed.errors[0].attr,
            "nixosConfigurations.host.config.system.build.toplevel"
        );
        assert!(failed.deferred.is_empty());
    }

    #[test]
    fn entry_point_longest_prefix_match() {
        let pats = vec!["packages.**".into(), "packages.*.foo".into()];
        assert_eq!(
            entry_point_of("packages.x86_64-linux.hello", &["packages.**".into()]),
            "packages.**"
        );
        assert_eq!(
            entry_point_of("packages.x86_64-linux.foo", &pats),
            "packages.*.foo"
        );
        assert_eq!(entry_point_of("checks.x86_64-linux.t", &pats), "checks");
        assert_eq!(
            entry_point_of(
                "devShells.aarch64-linux.default",
                &["devShells.*-linux.default".into()]
            ),
            "devShells.*-linux.default"
        );
    }

    #[tokio::test]
    async fn pooled_fan_out_drains_all_items_and_propagates_errors() {
        let seen = Mutex::new(Vec::new());
        let seen_ref = &seen;
        pooled_fan_out(3, (0..10).collect(), |n| async move {
            lock(seen_ref).push(n);
            Ok(Vec::new())
        })
        .await
        .expect("no errors");
        let mut got = seen.into_inner().unwrap_or_default();
        got.sort_unstable();
        assert_eq!(got, (0..10).collect::<Vec<_>>());

        let err = pooled_fan_out(2, vec![1, 2, 3], |n| async move {
            anyhow::ensure!(n != 2, "boom on {n}");
            Ok(Vec::new())
        })
        .await
        .expect_err("error must propagate");
        assert!(err.to_string().contains("boom on 2"));
    }

    #[tokio::test]
    async fn pooled_fan_out_runs_follow_ups_while_the_slow_item_is_in_flight() {
        let seen = Mutex::new(Vec::new());
        let seen_ref = &seen;
        pooled_fan_out(2, vec![0, 1], |n| async move {
            if n == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            lock(seen_ref).push(n);
            Ok(if n == 1 { vec![10, 11] } else { Vec::new() })
        })
        .await
        .expect("no errors");

        assert_eq!(seen.into_inner().unwrap_or_default(), vec![1, 10, 11, 0]);
    }
}
