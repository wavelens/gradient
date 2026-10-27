/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use tokio::sync::watch;

use super::failure::JobAborted;

// A dropped sender reads as an abort: the session that owned the job is gone and nobody can
// report its result or stop it later.
#[derive(Clone, Debug)]
pub struct AbortSignal(watch::Receiver<bool>);

impl AbortSignal {
    pub fn channel() -> (watch::Sender<bool>, Self) {
        let (tx, rx) = watch::channel(false);
        (tx, Self(rx))
    }

    #[cfg(test)]
    pub fn never() -> Self {
        let (tx, signal) = Self::channel();
        std::mem::forget(tx);
        signal
    }

    pub fn is_aborted(&self) -> bool {
        *self.0.borrow() || self.0.has_changed().is_err()
    }

    pub fn check(&self) -> Result<()> {
        if self.is_aborted() {
            return Err(JobAborted("job aborted by server".to_owned()).into());
        }
        Ok(())
    }

    pub async fn aborted(&mut self) {
        let _ = self.0.wait_for(|aborted| *aborted).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_sender_that_never_fires_is_not_an_abort() {
        let (_tx, signal) = AbortSignal::channel();
        assert!(!signal.is_aborted());
        assert!(signal.check().is_ok());
    }

    #[test]
    fn a_sent_abort_is_an_abort() {
        let (tx, signal) = AbortSignal::channel();
        tx.send(true).unwrap();
        assert!(signal.is_aborted());
        let err = signal.check().unwrap_err();
        assert!(err.downcast_ref::<JobAborted>().is_some(), "{err:#}");
    }

    #[test]
    fn a_dropped_sender_is_an_abort() {
        let (tx, signal) = AbortSignal::channel();
        drop(tx);
        assert!(signal.is_aborted());
        assert!(signal.check().is_err());
    }

    #[tokio::test]
    async fn waiting_ends_when_the_sender_is_dropped() {
        let (tx, mut signal) = AbortSignal::channel();
        let mut wait = std::pin::pin!(signal.aborted());
        assert!(futures::poll!(wait.as_mut()).is_pending());
        drop(tx);

        tokio::time::timeout(std::time::Duration::from_secs(5), wait)
            .await
            .expect("a dropped sender ends the wait");
    }
}
