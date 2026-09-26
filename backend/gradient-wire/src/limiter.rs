/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Concurrency cap for inbound `/proto` WebSocket connections.
//!
//! Wraps a [`tokio::sync::Semaphore`] sized from
//! `config.proto.max_proto_connections`. The proto upgrade handler tries to
//! acquire one permit per connection and holds it for the lifetime of the
//! session; when no permits are available the upgrade is rejected with 503
//! instead of queueing, so a misbehaving worker fan-out cannot exhaust file
//! descriptors, memory, or scheduler slots.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone)]
pub struct ProtoLimiter {
    semaphore: Arc<Semaphore>,
    capacity: usize,
}

impl ProtoLimiter {
    /// Build a limiter sized for `capacity` simultaneous connections. A
    /// configured value of `0` is clamped to `1` so the proto endpoint never
    /// silently rejects every upgrade - operators who want to disable the
    /// endpoint should set `discoverable = false` instead.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(capacity)),
            capacity,
        }
    }

    /// Try to claim a slot. Returns `Some(permit)` if a slot was free and
    /// `None` if the configured cap has been hit. The caller must keep the
    /// permit alive for the entire connection - dropping it returns the slot
    /// to the pool.
    pub fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.semaphore).try_acquire_owned().ok()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of slots currently held by live connections.
    pub fn in_use(&self) -> usize {
        self.capacity - self.semaphore.available_permits()
    }
}

/// Per-IP concurrency cap for anonymous `/proto` sessions. Each client IP gets
/// its own `Semaphore` sized to `max_per_ip`; entries are held via `Weak` so an
/// IP's slot map drops once all its connections close. Dead entries are pruned
/// opportunistically on insert to keep the map bounded.
#[derive(Debug)]
pub struct PerIpLimiter {
    inner: Mutex<HashMap<IpAddr, Weak<Semaphore>>>,
    max_per_ip: usize,
}

impl PerIpLimiter {
    pub fn new(max_per_ip: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_per_ip: max_per_ip.max(1),
        }
    }

    /// Try to claim a slot for `ip`. Returns `None` when that IP already holds
    /// `max_per_ip` live connections. The caller must keep the permit alive for
    /// the connection's lifetime.
    pub fn try_acquire(&self, ip: IpAddr) -> Option<OwnedSemaphorePermit> {
        let mut map = self.inner.lock().expect("per-ip limiter mutex poisoned");
        let semaphore = match map.get(&ip).and_then(Weak::upgrade) {
            Some(existing) => existing,
            None => {
                map.retain(|_, weak| weak.strong_count() > 0);
                let fresh = Arc::new(Semaphore::new(self.max_per_ip));
                map.insert(ip, Arc::downgrade(&fresh));
                fresh
            }
        };
        semaphore.try_acquire_owned().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_clamps_zero_capacity_to_one() {
        let limiter = ProtoLimiter::new(0);
        assert_eq!(limiter.capacity(), 1);
        assert!(
            limiter.try_acquire().is_some(),
            "clamped limiter must still admit one connection"
        );
    }

    #[test]
    fn try_acquire_returns_none_when_exhausted() {
        let limiter = ProtoLimiter::new(2);
        let p1 = limiter.try_acquire().expect("first permit");
        let p2 = limiter.try_acquire().expect("second permit");
        assert!(
            limiter.try_acquire().is_none(),
            "third acquire must fail when capacity is 2"
        );
        assert_eq!(limiter.in_use(), 2);
        drop(p1);
        drop(p2);
    }

    #[test]
    fn dropping_permit_releases_slot() {
        let limiter = ProtoLimiter::new(1);
        let permit = limiter.try_acquire().expect("first permit");
        assert!(limiter.try_acquire().is_none());
        drop(permit);
        assert!(
            limiter.try_acquire().is_some(),
            "slot must reopen once the prior permit is dropped"
        );
    }

    #[test]
    fn in_use_tracks_held_permits() {
        let limiter = ProtoLimiter::new(3);
        assert_eq!(limiter.in_use(), 0);
        let p1 = limiter.try_acquire().expect("first permit");
        assert_eq!(limiter.in_use(), 1);
        let p2 = limiter.try_acquire().expect("second permit");
        assert_eq!(limiter.in_use(), 2);
        drop(p1);
        assert_eq!(limiter.in_use(), 1);
        drop(p2);
        assert_eq!(limiter.in_use(), 0);
    }

    #[test]
    fn per_ip_caps_concurrent() {
        let limiter = PerIpLimiter::new(2);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let other: IpAddr = "10.0.0.1".parse().unwrap();

        let p1 = limiter.try_acquire(ip).expect("first slot for ip");
        let p2 = limiter.try_acquire(ip).expect("second slot for ip");
        assert!(
            limiter.try_acquire(ip).is_none(),
            "third connection for the same ip must be rejected"
        );

        let other_permit = limiter.try_acquire(other);
        assert!(
            other_permit.is_some(),
            "a different ip has its own independent cap"
        );

        drop(p1);
        assert!(
            limiter.try_acquire(ip).is_some(),
            "dropping a permit frees a slot for that ip"
        );
        drop(p2);
        drop(other_permit);
    }
}
