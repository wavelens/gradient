/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod connection;
mod target;
#[cfg(test)]
mod tests;

use connection::SshCommandStream;
use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use std::cell::RefCell;
use std::sync::Once;
use target::SshTarget;

const SCHEMES: [&str; 3] = ["ssh", "ssh+git", "git+ssh"];

thread_local! {
    static PRIVATE_KEY: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn with_private_key<T>(private_key: Option<&str>, operation: impl FnOnce() -> T) -> T {
    register();
    let previous = PRIVATE_KEY.replace(private_key.map(str::to_owned));
    let _restore = Restore(previous);
    operation()
}

struct Restore(Option<String>);

impl Drop for Restore {
    fn drop(&mut self) {
        PRIVATE_KEY.set(self.0.take());
    }
}

fn register() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        for scheme in SCHEMES {
            // SAFETY: the Once keeps registration away from transports created by this crate.
            if let Err(error) = unsafe { git2::transport::register(scheme, transport) } {
                tracing::error!(%error, scheme, "failed to register the SSH transport");
            }
        }
    });
}

fn transport(remote: &git2::Remote<'_>) -> Result<Transport, git2::Error> {
    let private_key = PRIVATE_KEY.with_borrow(Clone::clone);
    Transport::smart(remote, false, SshSubtransport { private_key })
}

struct SshSubtransport {
    private_key: Option<String>,
}

impl SmartSubtransport for SshSubtransport {
    fn action(
        &self,
        url: &str,
        action: Service,
    ) -> Result<Box<dyn SmartSubtransportStream>, git2::Error> {
        let program = match action {
            Service::UploadPackLs => "git-upload-pack",
            Service::ReceivePackLs => "git-receive-pack",
            Service::UploadPack | Service::ReceivePack => {
                return Err(git2::Error::from_str("SSH stream requested twice"));
            }
        };

        let private_key = self
            .private_key
            .as_deref()
            .ok_or_else(|| git2::Error::from_str("SSH remote needs a private key"))?;
        let key = russh::keys::decode_secret_key(private_key, None)
            .map_err(|e| git2::Error::from_str(&format!("invalid SSH private key: {e}")))?;
        let target = SshTarget::parse(url).map_err(|e| git2::Error::from_str(&e.to_string()))?;
        let command = target.command(program);

        SshCommandStream::open(&target, key, command)
            .map(|stream| Box::new(stream) as Box<dyn SmartSubtransportStream>)
            .map_err(|e| git2::Error::from_str(&format!("SSH to {}: {e}", target.host)))
    }

    fn close(&self) -> Result<(), git2::Error> {
        Ok(())
    }
}
