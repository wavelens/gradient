// SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

//! Interfaces to nix-daemon (or compatible) Stores.
//! ------------------------------------------------
//!
//! This module currently implements support for Protocol 1.35, and Nix 2.15+.
//!
//! Support for older versions will be added in the future - in particular, Protocol 1.21
//! used by Nix 2.3.

pub mod internal_json;
pub mod wire;

use crate::{
    BuildMode, ClientSettings, Error, PathInfo, Progress, Result, ResultExt, Stderr, Store,
};
use std::future::Future;
use std::{collections::HashMap, fmt::Debug};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use tokio_stream::StreamExt;
use tracing::{info, instrument};

/// Minimum supported protocol version. Older versions will be rejected.
///
/// Protocol 1.35 was introduced in Nix 2.15:
/// https://github.com/NixOS/nix/blob/2.15.0/src/libstore/worker-protocol.hh#L13
///
/// TODO: Support Protocol 1.21, used by Nix 2.3.
const MIN_PROTO: Proto = Proto(1, 35); // Nix >= 2.15.x

/// Maxmimum supported protocol version. Newer daemons will run in compatibility mode.
///
/// Protocol 1.35 is current as of Nix 2.19:
/// https://github.com/NixOS/nix/blob/2.19.3/src/libstore/worker-protocol.hh#L12
const MAX_PROTO: Proto = Proto(1, 35); // Nix <= 2.19.x

/// Protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Proto(u8, u8);

impl From<u64> for Proto {
    fn from(raw: u64) -> Self {
        Self(((raw & 0xFF00) >> 8) as u8, (raw & 0x00FF) as u8)
    }
}
impl From<Proto> for u64 {
    fn from(v: Proto) -> Self {
        ((v.0 as u64) << 8) | (v.1 as u64)
    }
}

impl std::fmt::Display for Proto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.0, self.1)
    }
}

impl Proto {
    fn since(&self, v: u8) -> bool {
        self.1 >= v
    }
}

trait DaemonProgressCaller {
    fn call<
        E: From<Error> + From<std::io::Error> + Send + Sync,
        C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    >(
        self,
        store: &mut DaemonStore<C>,
    ) -> impl Future<Output = Result<(), E>> + Send;
}

trait DaemonProgressReturner {
    type T: Send;
    fn result<
        E: From<Error> + From<std::io::Error> + Send + Sync,
        C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    >(
        self,
        store: &mut DaemonStore<C>,
    ) -> impl Future<Output = Result<Self::T, E>> + Send;
}

/// Internal [`crate::Progress`] implementation used by [`DaemonStore`].
struct DaemonProgress<'s, PC, PR, T: Send, C>
where
    C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    PC: DaemonProgressCaller + Send,
    PR: DaemonProgressReturner<T = T> + Send,
{
    store: &'s mut DaemonStore<C>,
    fuse: bool,
    caller: Option<PC>,
    returner: PR,
}
impl<'s, PC, PR, T: Send, C> DaemonProgress<'s, PC, PR, T, C>
where
    C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    PC: DaemonProgressCaller + Send,
    PR: DaemonProgressReturner<T = T> + Send,
{
    fn new(store: &'s mut DaemonStore<C>, caller: PC, returner: PR) -> Self {
        Self {
            store,
            fuse: false,
            caller: Some(caller),
            returner,
        }
    }
}
impl<'s, PC, PR, T: Send, C> Progress for DaemonProgress<'s, PC, PR, T, C>
where
    C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    PC: DaemonProgressCaller + Send,
    PR: DaemonProgressReturner<T = T> + Send,
{
    type T = T;
    type Error = Error;

    async fn next(&mut self) -> Result<Option<Stderr>> {
        let store = &mut self.store;
        if let Some(caller) = self.caller.take() {
            caller.call::<Error, C>(store).await?;
        }
        if self.fuse {
            Ok(None)
        } else {
            match wire::read_stderr(&mut store.conn).await? {
                Some(Stderr::Error(err)) => Err(Error::NixError(err)),
                Some(stderr) => Ok(Some(stderr)),
                None => {
                    self.fuse = true;
                    Ok(None)
                }
            }
        }
    }

    async fn result(mut self) -> Result<Self::T> {
        while let Some(_) = self.next().await? {}
        self.returner.result(self.store).await
    }
}

#[derive(Debug, Default)]
pub struct DaemonStoreBuilder {
    // This will do things in the future.
}

impl DaemonStoreBuilder {
    /// Initializes a [`DaemonStore`] by adopting a connection.
    ///
    /// It's up to the caller that the connection is in a state to begin a nix handshake, eg.
    /// it behaves like a fresh connection to the daemon socket - if this is a connection through
    /// a proxy, any proxy handshakes should already have taken place, etc.
    ///
    /// ```no_run
    /// use tokio::net::UnixStream;
    /// use nix_daemon::nix::DaemonStore;
    ///
    /// # async {
    /// let conn = UnixStream::connect("/nix/var/nix/daemon-socket/socket").await?;
    /// let store = DaemonStore::builder().init(conn).await?;
    /// # Ok::<_, nix_daemon::Error>(())
    /// # };
    /// ```
    pub async fn init<C: AsyncReadExt + AsyncWriteExt + Unpin>(
        self,
        conn: C,
    ) -> Result<DaemonStore<C>> {
        let mut store = DaemonStore {
            conn,
            buffer: [0u8; 32 * 1024],
            proto: Proto(0, 0),
        };
        store.handshake().await?;
        Ok(store)
    }

    /// Connects to a Nix daemon via a unix socket.
    /// The path is usually `/nix/var/nix/daemon-socket/socket`.
    ///
    /// ```no_run
    /// use nix_daemon::{Store, Progress, nix::DaemonStore};
    ///
    /// # async {
    /// let store = DaemonStore::builder()
    ///     .connect_unix("/nix/var/nix/daemon-socket/socket")
    ///     .await?;
    /// # Ok::<_, nix_daemon::Error>(())
    /// # };
    /// ```
    pub async fn connect_unix<P: AsRef<std::path::Path>>(
        self,
        path: P,
    ) -> Result<DaemonStore<UnixStream>> {
        self.init(UnixStream::connect(path).await?).await
    }
}

/// Store backed by a `nix-daemon` (or compatible store). Implements [`crate::Store`].
///
/// ```no_run
/// use nix_daemon::{Store, Progress, nix::DaemonStore};
///
/// # async {
/// let mut store = DaemonStore::builder()
///     .connect_unix("/nix/var/nix/daemon-socket/socket")
///     .await?;
///
/// let is_valid_path = store.is_valid_path("/nix/store/...").result().await?;
/// # Ok::<_, nix_daemon::Error>(())
/// # };
/// ```
#[derive(Debug)]
pub struct DaemonStore<C: AsyncReadExt + AsyncWriteExt + Unpin> {
    conn: C,
    buffer: [u8; 32 * 1024],
    /// Negotiated protocol version.
    pub proto: Proto,
}

impl DaemonStore<UnixStream> {
    /// Returns a Builder.
    pub fn builder() -> DaemonStoreBuilder {
        DaemonStoreBuilder::default()
    }
}

impl<C: AsyncReadExt + AsyncWriteExt + Unpin> DaemonStore<C> {
    #[instrument(skip(self))]
    async fn handshake(&mut self) -> Result<()> {
        // Exchange magic numbers.
        wire::write_u64(&mut self.conn, wire::WORKER_MAGIC_1)
            .await
            .with_field("magic1")?;
        match wire::read_u64(&mut self.conn).await {
            Ok(magic2 @ wire::WORKER_MAGIC_2) => Ok(magic2),
            Ok(v) => Err(Error::Invalid(format!("{:#x}", v))),
            Err(err) => Err(err.into()),
        }
        .with_field("magic2")?;

        // Check that we're talking to a new enough daemon, tell them our version.
        self.proto = wire::read_proto(&mut self.conn)
            .await
            .and_then(|proto| {
                if proto.0 != 1 || proto < MIN_PROTO {
                    return Err(Error::Invalid(format!("{}", proto)));
                }
                Ok(proto)
            })
            .with_field("daemon_proto")?;
        wire::write_proto(&mut self.conn, MAX_PROTO)
            .await
            .with_field("client_proto")?;

        // Write some obsolete fields.
        if self.proto >= Proto(1, 14) {
            wire::write_u64(&mut self.conn, 0)
                .await
                .with_field("__obsolete_cpu_affinity")?;
        }
        if self.proto >= Proto(1, 11) {
            wire::write_bool(&mut self.conn, false)
                .await
                .with_field("__obsolete_reserve_space")?;
        }

        // And we don't currently do anything with these.
        if self.proto >= Proto(1, 33) {
            wire::read_string(&mut self.conn)
                .await
                .with_field("nix_version")?;
        }
        if self.proto >= Proto(1, 35) {
            // Option<bool>: 0 = None, 1 = Some(true), 2 = Some(false)
            wire::read_u64(&mut self.conn)
                .await
                .with_field("remote_trust")?;
        }

        // Discard Stderr. There shouldn't be anything here anyway.
        while let Some(_) = wire::read_stderr(&mut self.conn).await? {}
        Ok(())
    }
}

impl<C: AsyncReadExt + AsyncWriteExt + Unpin + Send> Store for DaemonStore<C> {
    type Error = Error;

    #[instrument(skip(self))]
    fn is_valid_path<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> impl Progress<T = bool, Error = Self::Error> {
        // We have to do this silly verbose thing, because using two closures and passing
        // &mut store to both, summons incomprehensible horrors into your lifetimes that
        // have already taken days of my life.
        //
        // Also I don't think I can match all the type constraints some of the later ops
        // (AddToStore, I'm looking at you) with `macro_rules` in a way that lets me
        // synthesize these, but this is quite likely a skill issue.
        //
        // If you can figure it out, please, share with me your wisdom.
        struct Caller<S: AsRef<str> + Send + Sync + Debug> {
            path: S,
        }
        impl<S: AsRef<str> + Send + Sync + Debug> DaemonProgressCaller for Caller<S> {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::IsValidPath)
                    .await
                    .with_field("IsValidPath.<op>")?;
                wire::write_string(&mut store.conn, &self.path)
                    .await
                    .with_field("IsValidPath.path")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = bool;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                Ok(wire::read_bool(&mut store.conn).await?)
            }
        }

        DaemonProgress::new(self, Caller { path }, Returner)
    }

    #[instrument(skip(self))]
    fn has_substitutes<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Progress<T = bool, Error = Self::Error> {
        struct Caller<P: AsRef<str> + Send + Sync + Debug> {
            path: P,
        }
        impl<P: AsRef<str> + Send + Sync + Debug> DaemonProgressCaller for Caller<P> {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::HasSubstitutes)
                    .await
                    .with_field("HasSubstitutes.<op>")?;
                wire::write_string(&mut store.conn, &self.path)
                    .await
                    .with_field("HasSubstitutes.path")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = bool;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                Ok(wire::read_bool(&mut store.conn).await?)
            }
        }

        DaemonProgress::new(self, Caller { path }, Returner)
    }

    #[instrument(skip(self, source))]
    fn add_to_store<
        SN: AsRef<str> + Send + Sync + Debug,
        SC: AsRef<str> + Send + Sync + Debug,
        Refs,
        R,
    >(
        &mut self,
        name: SN,
        cam_str: SC,
        refs: Refs,
        repair: bool,
        source: R,
    ) -> impl Progress<T = (String, PathInfo), Error = Self::Error>
    where
        Refs: IntoIterator + Send + Debug,
        Refs::IntoIter: ExactSizeIterator + Send,
        Refs::Item: AsRef<str> + Send + Sync,
        R: AsyncReadExt + Unpin + Send + Debug,
    {
        struct Caller<
            SN: AsRef<str> + Send + Sync + Debug,
            SC: AsRef<str> + Send + Sync + Debug,
            Refs,
            R,
        >
        where
            Refs: IntoIterator + Send + Debug,
            Refs::IntoIter: ExactSizeIterator + Send,
            Refs::Item: AsRef<str> + Send + Sync,
            R: AsyncReadExt + Unpin + Send + Debug,
        {
            name: SN,
            cam_str: SC,
            refs: Refs,
            repair: bool,
            source: R,
        }
        impl<SN: AsRef<str> + Send + Sync + Debug, SC: AsRef<str> + Send + Sync + Debug, Refs, R>
            DaemonProgressCaller for Caller<SN, SC, Refs, R>
        where
            Refs: IntoIterator + Send + Debug,
            Refs::IntoIter: ExactSizeIterator + Send,
            Refs::Item: AsRef<str> + Send + Sync,
            R: AsyncReadExt + Unpin + Send + Debug,
        {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                mut self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                match store.proto {
                    Proto(1, 25..) => {
                        wire::write_op(&mut store.conn, wire::Op::AddToStore)
                            .await
                            .with_field("AddToStore.<op>")?;
                        wire::write_string(&mut store.conn, self.name)
                            .await
                            .with_field("AddToStore.name")?;
                        wire::write_string(&mut store.conn, self.cam_str)
                            .await
                            .with_field("AddToStore.camStr")?;
                        wire::write_strings(&mut store.conn, self.refs)
                            .await
                            .with_field("AddToStore.refs")?;
                        wire::write_bool(&mut store.conn, self.repair)
                            .await
                            .with_field("AddToStore.repair")?;
                        wire::copy_to_framed(&mut self.source, &mut store.conn, &mut store.buffer)
                            .await
                            .with_field("AddToStore.<source>")?;
                        Ok(())
                    }
                    proto => Err(Error::Invalid(format!(
                        "AddToStore is not implemented for Protocol {}",
                        proto
                    ))
                    .into()),
                }
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = (String, PathInfo);
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                Ok((
                    wire::read_string(&mut store.conn)
                        .await
                        .with_field("name")?,
                    wire::read_pathinfo(&mut store.conn, store.proto)
                        .await
                        .with_field("PathInfo")?,
                ))
            }
        }

        DaemonProgress::new(
            self,
            Caller {
                name,
                cam_str,
                refs,
                repair,
                source,
            },
            Returner,
        )
    }

    #[instrument(skip(self))]
    fn build_paths<Paths>(
        &mut self,
        paths: Paths,
        mode: BuildMode,
    ) -> impl Progress<T = (), Error = Self::Error>
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync,
    {
        struct Caller<Paths>
        where
            Paths: IntoIterator + Send + Debug,
            Paths::IntoIter: ExactSizeIterator + Send,
            Paths::Item: AsRef<str> + Send + Sync,
        {
            paths: Paths,
            mode: BuildMode,
        }
        impl<Paths> DaemonProgressCaller for Caller<Paths>
        where
            Paths: IntoIterator + Send + Debug,
            Paths::IntoIter: ExactSizeIterator + Send,
            Paths::Item: AsRef<str> + Send + Sync,
        {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::BuildPaths)
                    .await
                    .with_field("BuildPaths.<op>")?;
                wire::write_strings(&mut store.conn, self.paths)
                    .await
                    .with_field("BuildPaths.paths")?;
                if store.proto >= Proto(1, 15) {
                    wire::write_build_mode(&mut store.conn, self.mode)
                        .await
                        .with_field("BuildPaths.build_mode")?;
                }
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = ();
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                wire::read_u64(&mut store.conn)
                    .await
                    .with_field("__unused__")?;
                Ok(())
            }
        }

        DaemonProgress::new(self, Caller { paths, mode }, Returner)
    }

    #[instrument(skip(self))]
    fn ensure_path<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> impl Progress<T = (), Error = Self::Error> {
        struct Caller<Path: AsRef<str> + Send + Sync + Debug> {
            path: Path,
        }
        impl<Path: AsRef<str> + Send + Sync + Debug> DaemonProgressCaller for Caller<Path> {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::EnsurePath)
                    .await
                    .with_field("EnsurePath.<op>")?;
                wire::write_string(&mut store.conn, self.path)
                    .await
                    .with_field("EnsurePath.path")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = ();
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                wire::read_u64(&mut store.conn)
                    .await
                    .with_field("__unused__")?;
                Ok(())
            }
        }

        DaemonProgress::new(self, Caller { path }, Returner)
    }

    #[instrument(skip(self))]
    fn add_temp_root<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> impl Progress<T = (), Error = Self::Error> {
        struct Caller<Path: AsRef<str> + Send + Sync + Debug> {
            path: Path,
        }
        impl<Path: AsRef<str> + Send + Sync + Debug> DaemonProgressCaller for Caller<Path> {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::AddTempRoot)
                    .await
                    .with_field("AddTempRoot.<op>")?;
                wire::write_string(&mut store.conn, self.path)
                    .await
                    .with_field("AddTempRoot.path")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = ();
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                wire::read_u64(&mut store.conn)
                    .await
                    .with_field("__unused__")?;
                Ok(())
            }
        }

        DaemonProgress::new(self, Caller { path }, Returner)
    }

    #[instrument(skip(self))]
    fn add_indirect_root<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> impl Progress<T = (), Error = Self::Error> {
        struct Caller<Path: AsRef<str> + Send + Sync + Debug> {
            path: Path,
        }
        impl<Path: AsRef<str> + Send + Sync + Debug> DaemonProgressCaller for Caller<Path> {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::AddIndirectRoot)
                    .await
                    .with_field("AddIndirectRoot.<op>")?;
                wire::write_string(&mut store.conn, self.path)
                    .await
                    .with_field("AddIndirectRoot.path")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = ();
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                wire::read_u64(&mut store.conn)
                    .await
                    .with_field("__unused__")?;
                Ok(())
            }
        }

        DaemonProgress::new(self, Caller { path }, Returner)
    }

    #[instrument(skip(self))]
    fn find_roots(&mut self) -> impl Progress<T = HashMap<String, String>, Error = Self::Error> {
        struct Caller;
        impl DaemonProgressCaller for Caller {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::FindRoots)
                    .await
                    .with_field("FindRoots.<op>")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = HashMap<String, String>;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                let count = wire::read_u64(&mut store.conn)
                    .await
                    .with_field("FindRoots.roots[].<count>")?;
                let mut roots = HashMap::with_capacity(count as usize);
                for _ in 0..count {
                    let link = wire::read_string(&mut store.conn)
                        .await
                        .with_field("FindRoots.roots[].link")?;
                    let target = wire::read_string(&mut store.conn)
                        .await
                        .with_field("FindRoots.roots[].target")?;
                    roots.insert(link, target);
                }
                Ok(roots)
            }
        }

        DaemonProgress::new(self, Caller, Returner)
    }

    #[instrument(skip(self))]
    fn set_options(&mut self, opts: ClientSettings) -> impl Progress<T = (), Error = Self::Error> {
        struct Caller {
            opts: ClientSettings,
        }
        impl DaemonProgressCaller for Caller {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::SetOptions)
                    .await
                    .with_field("SetOptions.<op>")?;
                wire::write_client_settings(&mut store.conn, store.proto, &self.opts)
                    .await
                    .with_field("SetOptions.clientSettings")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = ();
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                _store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                Ok(())
            }
        }

        DaemonProgress::new(self, Caller { opts }, Returner)
    }

    #[instrument(skip(self))]
    fn query_pathinfo<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> impl Progress<T = Option<PathInfo>, Error = Self::Error> {
        struct Caller<S: AsRef<str> + Send + Sync + Debug> {
            path: S,
        }
        impl<S: AsRef<str> + Send + Sync + Debug> DaemonProgressCaller for Caller<S> {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::QueryPathInfo)
                    .await
                    .with_field("QueryPathInfo.<op>")?;
                wire::write_string(&mut store.conn, &self.path)
                    .await
                    .with_field("QueryPathInfo.path")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = Option<PathInfo>;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                if wire::read_bool(&mut store.conn).await? {
                    Ok(Some(
                        wire::read_pathinfo(&mut store.conn, store.proto).await?,
                    ))
                } else {
                    Ok(None)
                }
            }
        }

        DaemonProgress::new(self, Caller { path }, Returner)
    }

    #[instrument(skip(self))]
    fn query_valid_paths<Paths>(
        &mut self,
        paths: Paths,
        use_substituters: bool,
    ) -> impl Progress<T = Vec<String>, Error = Self::Error>
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync,
    {
        struct Caller<Paths>
        where
            Paths: IntoIterator + Send + Debug,
            Paths::IntoIter: ExactSizeIterator + Send,
            Paths::Item: AsRef<str> + Send + Sync,
        {
            paths: Paths,
            use_substituters: bool,
        }
        impl<Paths> DaemonProgressCaller for Caller<Paths>
        where
            Paths: IntoIterator + Send + Debug,
            Paths::IntoIter: ExactSizeIterator + Send,
            Paths::Item: AsRef<str> + Send + Sync,
        {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                match store.proto {
                    Proto(1, 12..) => {
                        wire::write_op(&mut store.conn, wire::Op::QueryValidPaths)
                            .await
                            .with_field("QueryValidPaths.<op>")?;
                        wire::write_strings(&mut store.conn, self.paths)
                            .await
                            .with_field("QueryValidPaths.path")?;
                        if store.proto >= Proto(1, 27) {
                            wire::write_bool(&mut store.conn, self.use_substituters)
                                .await
                                .with_field("QueryValidPaths.use_substituters")?;
                        }
                        Ok(())
                    }
                    proto => Err(Error::Invalid(format!(
                        "QueryValidPaths is not implemented for Protocol {}",
                        proto
                    ))
                    .into()),
                }
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = Vec<String>;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                Ok(wire::read_strings(&mut store.conn)
                    .collect::<Result<Vec<_>>>()
                    .await?)
            }
        }

        DaemonProgress::new(
            self,
            Caller {
                paths,
                use_substituters,
            },
            Returner,
        )
    }

    #[instrument(skip(self))]
    fn query_substitutable_paths<Paths>(
        &mut self,
        paths: Paths,
    ) -> impl Progress<T = Vec<String>, Error = Self::Error>
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync,
    {
        struct Caller<Paths>
        where
            Paths: IntoIterator + Send + Debug,
            Paths::IntoIter: ExactSizeIterator + Send,
            Paths::Item: AsRef<str> + Send + Sync,
        {
            paths: Paths,
        }
        impl<Paths> DaemonProgressCaller for Caller<Paths>
        where
            Paths: IntoIterator + Send + Debug,
            Paths::IntoIter: ExactSizeIterator + Send,
            Paths::Item: AsRef<str> + Send + Sync,
        {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::QuerySubstitutablePaths)
                    .await
                    .with_field("QuerySubstitutablePaths.<op>")?;
                wire::write_strings(&mut store.conn, self.paths)
                    .await
                    .with_field("QuerySubstitutablePaths.path")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = Vec<String>;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                Ok(wire::read_strings(&mut store.conn)
                    .collect::<Result<Vec<_>>>()
                    .await?)
            }
        }

        DaemonProgress::new(self, Caller { paths }, Returner)
    }

    #[instrument(skip(self))]
    fn query_valid_derivers<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> impl Progress<T = Vec<String>, Error = Self::Error> {
        struct Caller<S: AsRef<str> + Send + Sync + Debug> {
            path: S,
        }
        impl<S: AsRef<str> + Send + Sync + Debug> DaemonProgressCaller for Caller<S> {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::QueryValidDerivers)
                    .await
                    .with_field("QueryValidDerivers.<op>")?;
                wire::write_string(&mut store.conn, &self.path)
                    .await
                    .with_field("QueryValidDerivers.path")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = Vec<String>;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                Ok(wire::read_strings(&mut store.conn)
                    .collect::<Result<Vec<String>>>()
                    .await
                    .with_field("QueryValidDerivers.paths")?)
            }
        }

        DaemonProgress::new(self, Caller { path }, Returner)
    }

    #[instrument(skip(self))]
    fn query_missing<Ps>(
        &mut self,
        paths: Ps,
    ) -> impl Progress<T = crate::Missing, Error = Self::Error>
    where
        Ps: IntoIterator + Send + Debug,
        Ps::IntoIter: ExactSizeIterator + Send,
        Ps::Item: AsRef<str> + Send + Sync,
    {
        struct Caller<Ps>
        where
            Ps: IntoIterator + Send + Debug,
            Ps::IntoIter: ExactSizeIterator + Send,
            Ps::Item: AsRef<str> + Send + Sync,
        {
            paths: Ps,
        }
        impl<Ps> DaemonProgressCaller for Caller<Ps>
        where
            Ps: IntoIterator + Send + Debug,
            Ps::IntoIter: ExactSizeIterator + Send,
            Ps::Item: AsRef<str> + Send + Sync,
        {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::QueryMissing)
                    .await
                    .with_field("QueryMissing.<op>")?;
                wire::write_strings(&mut store.conn, self.paths)
                    .await
                    .with_field("QueryMissing.paths")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = crate::Missing;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                let will_build = wire::read_strings(&mut store.conn)
                    .collect::<Result<Vec<String>>>()
                    .await
                    .with_field("QueryMissing.will_build")?;
                let will_substitute = wire::read_strings(&mut store.conn)
                    .collect::<Result<Vec<String>>>()
                    .await
                    .with_field("QueryMissing.will_substitute")?;
                let unknown = wire::read_strings(&mut store.conn)
                    .collect::<Result<Vec<String>>>()
                    .await
                    .with_field("QueryMissing.unknown")?;
                let download_size = wire::read_u64(&mut store.conn)
                    .await
                    .with_field("QueryMissing.download_size")?;
                let nar_size = wire::read_u64(&mut store.conn)
                    .await
                    .with_field("QueryMissing.nar_size")?;
                Ok(crate::Missing {
                    will_build,
                    will_substitute,
                    unknown,
                    download_size,
                    nar_size,
                })
            }
        }

        DaemonProgress::new(self, Caller { paths }, Returner)
    }

    #[instrument(skip(self))]
    fn query_derivation_output_map<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Progress<T = HashMap<String, String>, Error = Self::Error> {
        struct Caller<P: AsRef<str> + Send + Sync + Debug> {
            path: P,
        }
        impl<P: AsRef<str> + Send + Sync + Debug> DaemonProgressCaller for Caller<P> {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::QueryDerivationOutputMap)
                    .await
                    .with_field("QueryDerivationOutputMap.<op>")?;
                wire::write_string(&mut store.conn, self.path)
                    .await
                    .with_field("QueryDerivationOutputMap.paths")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = HashMap<String, String>;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                let mut outputs = HashMap::new();
                let count = wire::read_u64(&mut store.conn)
                    .await
                    .with_field("QueryDerivationOutputMap.outputs[].<count>")?;
                for _ in 0..count {
                    let name = wire::read_string(&mut store.conn)
                        .await
                        .with_field("QueryDerivationOutputMap.outputs[].name")?;
                    let path = wire::read_string(&mut store.conn)
                        .await
                        .with_field("QueryDerivationOutputMap.outputs[].path")?;
                    outputs.insert(name, path);
                }
                Ok(outputs)
            }
        }

        DaemonProgress::new(self, Caller { path }, Returner)
    }

    #[instrument(skip(self))]
    fn build_paths_with_results<Ps>(
        &mut self,
        paths: Ps,
        mode: BuildMode,
    ) -> impl Progress<T = HashMap<String, crate::BuildResult>, Error = Self::Error>
    where
        Ps: IntoIterator + Send + Debug,
        Ps::IntoIter: ExactSizeIterator + Send,
        Ps::Item: AsRef<str> + Send + Sync,
    {
        struct Caller<Ps>
        where
            Ps: IntoIterator + Send + Debug,
            Ps::IntoIter: ExactSizeIterator + Send,
            Ps::Item: AsRef<str> + Send + Sync,
        {
            paths: Ps,
            mode: BuildMode,
        }
        impl<Ps> DaemonProgressCaller for Caller<Ps>
        where
            Ps: IntoIterator + Send + Debug,
            Ps::IntoIter: ExactSizeIterator + Send,
            Ps::Item: AsRef<str> + Send + Sync,
        {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::BuildPathsWithResults)
                    .await
                    .with_field("BuildPathsWithResults.<op>")?;
                wire::write_strings(&mut store.conn, self.paths)
                    .await
                    .with_field("BuildPathsWithResults.paths")?;
                wire::write_build_mode(&mut store.conn, self.mode)
                    .await
                    .with_field("BuildPathsWithResults.build_mode")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = HashMap<String, crate::BuildResult>;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                let count = wire::read_u64(&mut store.conn)
                    .await
                    .with_field("BuildPathsWithResults.results.<count>")?;
                let mut results = HashMap::with_capacity(count as usize);
                for _ in 0..count {
                    let path = wire::read_string(&mut store.conn)
                        .await
                        .with_field("BuildPathsWithResults.results[].path")?;
                    let result = wire::read_build_result(&mut store.conn, store.proto)
                        .await
                        .with_field("BuildPathsWithResults.results[].result")?;
                    results.insert(path, result);
                }
                Ok(results)
            }
        }
        DaemonProgress::new(self, Caller { paths, mode }, Returner)
    }

    #[instrument(skip(self))]
    fn nar_from_path<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Progress<T = Vec<String>, Error = Self::Error> {
        struct Caller<P>
        where
            P: AsRef<str> + Send + Sync + Debug,
        {
            path: P,
        }
        impl<P> DaemonProgressCaller for Caller<P>
        where
            P: AsRef<str> + Send + Sync + Debug,
        {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                wire::write_op(&mut store.conn, wire::Op::NarFromPath)
                    .await
                    .with_field("NarFromPath.<op>")?;
                wire::write_string(&mut store.conn, self.path)
                    .await
                    .with_field("NarFromPath.path")?;
                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = Vec<String>;
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                let mut nar_data = Vec::new();
                let mut paren_count = 0;
                let mut found_start = false;

                // First check for narVersionMagic1
                let magic = wire::read_string(&mut store.conn).await?;
                if magic != "nix-archive-1" {
                    return Err(
                        Error::Invalid(format!("Expected nix-archive-1, got {}", magic)).into(),
                    );
                }

                nar_data.push(magic);

                // Read framed data until we find the complete NAR structure
                loop {
                    let nar_string = wire::read_string(&mut store.conn).await?;
                    nar_data.push(nar_string.clone());

                    if nar_string == "(" {
                        paren_count += 1;
                        found_start = true;
                    } else if nar_string == ")" && found_start {
                        paren_count -= 1;
                        if paren_count == 0 {
                            return Ok(nar_data);
                        }
                    }
                }
            }
        }

        DaemonProgress::new(self, Caller { path }, Returner)
    }

    #[instrument(skip(self, nar_source))]
    fn add_to_store_nar<P: AsRef<str> + Send + Sync + Debug, R>(
        &mut self,
        path: P,
        path_info: PathInfo,
        nar_source: R,
    ) -> impl Progress<T = (), Error = Self::Error>
    where
        R: IntoIterator + Send + Debug,
        R::IntoIter: ExactSizeIterator + Send,
        R::Item: AsRef<str> + Send + Sync,
    {
        struct Caller<P, R>
        where
            P: AsRef<str> + Send + Sync + Debug,
            R: IntoIterator + Send + Debug,
            R::IntoIter: ExactSizeIterator + Send,
            R::Item: AsRef<str> + Send + Sync,
        {
            path: P,
            path_info: PathInfo,
            nar_source: R,
        }
        impl<P, R> DaemonProgressCaller for Caller<P, R>
        where
            P: AsRef<str> + Send + Sync + Debug,
            R: IntoIterator + Send + Debug,
            R::IntoIter: ExactSizeIterator + Send,
            R::Item: AsRef<str> + Send + Sync,
        {
            async fn call<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                store: &mut DaemonStore<C>,
            ) -> Result<(), E> {
                // Based on C++ RemoteStore::addToStore with ValidPathInfo
                // Check if this operation is supported by this protocol version
                if store.proto < Proto(1, 25) {
                    return Err(Error::Invalid(format!(
                        "AddToStoreNar not supported in protocol {}",
                        store.proto
                    ))
                    .into());
                }

                // Send AddToStoreNar operation - matches C++ exactly
                wire::write_op(&mut store.conn, wire::Op::AddToStoreNar)
                    .await
                    .with_field("AddToStoreNar.<op>")?;

                // C++ order: path, deriver, narHash, references, registrationTime, narSize, ultimate, sigs, ca, repair, !checkSigs
                wire::write_string(&mut store.conn, self.path)
                    .await
                    .with_field("AddToStoreNar.path")?;

                wire::write_string(&mut store.conn, self.path_info.deriver.unwrap_or_default())
                    .await
                    .with_field("AddToStoreNar.deriver")?;

                // In C++ this is: info.narHash.to_string(HashFormat::Base16, false)
                wire::write_string(&mut store.conn, &self.path_info.nar_hash)
                    .await
                    .with_field("AddToStoreNar.narHash")?;

                wire::write_strings(&mut store.conn, &self.path_info.references)
                    .await
                    .with_field("AddToStoreNar.references")?;

                wire::write_u64(
                    &mut store.conn,
                    self.path_info.registration_time.timestamp() as u64,
                )
                .await
                .with_field("AddToStoreNar.registrationTime")?;

                wire::write_u64(&mut store.conn, self.path_info.nar_size)
                    .await
                    .with_field("AddToStoreNar.narSize")?;

                wire::write_bool(&mut store.conn, self.path_info.ultimate)
                    .await
                    .with_field("AddToStoreNar.ultimate")?;

                wire::write_strings(&mut store.conn, &self.path_info.signatures)
                    .await
                    .with_field("AddToStoreNar.sigs")?;

                wire::write_string(&mut store.conn, self.path_info.ca.unwrap_or_default())
                    .await
                    .with_field("AddToStoreNar.ca")?;

                // repair and !checkSigs
                wire::write_bool(&mut store.conn, false)
                    .await
                    .with_field("AddToStoreNar.repair")?;

                wire::write_bool(&mut store.conn, true) // !checkSigs = true means don't check sigs
                    .await
                    .with_field("AddToStoreNar.dontCheckSigs")?;

                let mut nar_binary = Vec::new();
                for nar_string in self.nar_source {
                    let s = nar_string.as_ref();
                    let bytes = s.as_bytes();

                    nar_binary.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                    nar_binary.extend_from_slice(bytes);

                    let padding_needed = if bytes.len() % 8 > 0 {
                        8 - (bytes.len() % 8)
                    } else {
                        0
                    };
                    for _ in 0..padding_needed {
                        nar_binary.push(0);
                    }
                }

                wire::write_u64(&mut store.conn, nar_binary.len() as u64).await?;
                store.conn.write_all(&nar_binary).await?;
                wire::write_u64(&mut store.conn, 0).await?; // Terminating frame

                Ok(())
            }
        }

        struct Returner;
        impl DaemonProgressReturner for Returner {
            type T = ();
            async fn result<
                E: From<Error> + From<std::io::Error> + Send + Sync,
                C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
            >(
                self,
                _store: &mut DaemonStore<C>,
            ) -> Result<Self::T, E> {
                Ok(())
            }
        }

        DaemonProgress::new(
            self,
            Caller {
                path,
                path_info,
                nar_source,
            },
            Returner,
        )
    }
}

#[derive(Debug)]
pub struct DaemonProtocolAdapterBuilder<'s, S: Store> {
    pub store: &'s mut S,
    pub nix_version: String,
    pub remote_trust: Option<bool>,
}

impl<'s, S: Store> DaemonProtocolAdapterBuilder<'s, S> {
    fn new(store: &'s mut S) -> Self {
        Self {
            store,
            nix_version: concat!("gorgon/nix-daemon ", env!("CARGO_PKG_VERSION")).to_string(),
            remote_trust: None,
        }
    }

    /// Initializes a [`DaemonProtocolAdapter`] by adopting a connection.
    ///
    /// It's up to the caller that the connection is in a state to begin a nix handshake, eg.
    /// it behaves like a fresh connection to the daemon socket - if this is a connection through
    /// a proxy, any proxy handshakes should already have taken place, etc.
    pub async fn adopt<
        R: AsyncReadExt + Unpin + Send + Debug,
        W: AsyncWriteExt + Unpin + Send + Debug,
    >(
        self,
        r: R,
        w: W,
    ) -> Result<DaemonProtocolAdapter<'s, S, R, W>> {
        DaemonProtocolAdapter::handshake(r, w, self.store, self.nix_version, self.remote_trust)
            .await
    }
}

/// Handles an incoming `nix-daemon` protocol connection, and forwards calls to a
/// [`crate::Store`].
///
/// ```no_run
/// use tokio::net::UnixListener;
/// use nix_daemon::nix::{DaemonStore, DaemonProtocolAdapter};
///
/// # async {
/// // Accept a connection.
/// let listener = UnixListener::bind("/tmp/nix-proxy.sock")?;
/// let (conn, _addr) = listener.accept().await?;
///
/// // Connect to the real store.
/// let mut store = DaemonStore::builder()
///     .connect_unix("/nix/var/nix/daemon-socket/socket")
///     .await?;
///
/// // Proxy the connection to it.
/// let (cr, cw) = conn.into_split();
/// let mut adapter = DaemonProtocolAdapter::builder(&mut store)
///     .adopt(cr, cw)
///     .await?;
/// Ok::<(),nix_daemon::Error>(adapter.run().await?)
/// # };
/// ```
///
/// See [nix-supervisor](https://codeberg.org/gorgon/gorgon/src/branch/main/nix-supervisor) for
/// a more advanced example of how to use this (with a custom Store implementation).
pub struct DaemonProtocolAdapter<'s, S: Store, R, W>
where
    R: AsyncReadExt + Unpin + Send + Debug,
    W: AsyncWriteExt + Unpin + Send + Debug,
{
    r: R,
    w: W,
    store: &'s mut S,
    /// Negotiated protocol version.
    pub proto: Proto,
}

impl<'s, S: Store>
    DaemonProtocolAdapter<'s, S, tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>
{
    pub fn builder(store: &'s mut S) -> DaemonProtocolAdapterBuilder<'s, S> {
        DaemonProtocolAdapterBuilder::new(store)
    }
}

impl<'s, S: Store, R, W> DaemonProtocolAdapter<'s, S, R, W>
where
    R: AsyncReadExt + Unpin + Send + Debug,
    W: AsyncWriteExt + Unpin + Send + Debug,
{
    #[instrument(skip(r, w, store))]
    async fn handshake(
        mut r: R,
        mut w: W,
        store: &'s mut S,
        nix_version: String,
        remote_trust: Option<bool>,
    ) -> Result<Self> {
        // Exchange magic numbers.
        match wire::read_u64(&mut r).await {
            Ok(magic1 @ wire::WORKER_MAGIC_1) => Ok(magic1),
            Ok(v) => Err(Error::Invalid(format!("{:#x}", v))),
            Err(err) => Err(err.into()),
        }
        .with_field("magic1")?;
        wire::write_u64(&mut w, wire::WORKER_MAGIC_2)
            .await
            .with_field("magic2")?;

        // Tell the client our latest supported protocol version, then they pick that or lower.
        wire::write_proto(&mut w, MAX_PROTO)
            .await
            .with_field("daemon_proto")?;
        let proto = wire::read_proto(&mut r)
            .await
            .and_then(|proto| {
                if proto.0 != 1 || proto < MIN_PROTO {
                    return Err(Error::Invalid(format!("{}", proto)));
                }
                Ok(proto)
            })
            .with_field("client_proto")?;

        // Discard some obsolete fields.
        if proto >= Proto(1, 14) {
            wire::read_u64(&mut r)
                .await
                .with_field("__obsolete_cpu_affinity")?;
        }
        if proto >= Proto(1, 11) {
            wire::read_bool(&mut r)
                .await
                .with_field("__obsolete_reserve_space")?;
        }

        // And use values from the builder for these.
        if proto >= Proto(1, 33) {
            wire::write_string(&mut w, &nix_version)
                .await
                .with_field("nix_version")?;
        }
        if proto >= Proto(1, 35) {
            wire::write_u64(
                &mut w,
                match remote_trust {
                    None => 0,
                    Some(true) => 1,
                    Some(false) => 2,
                },
            )
            .await
            .with_field("remote_trust")?;
        }

        // Stderr is always empty.
        wire::write_stderr(&mut w, None)
            .await
            .with_field("stderr")?;
        Ok(Self { r, w, store, proto })
    }

    /// Runs the connection until the client disconnects. TODO: Cancellation.
    pub async fn run(&mut self) -> Result<(), S::Error> {
        loop {
            match wire::read_op(&mut self.r).await {
                Ok(wire::Op::IsValidPath) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("IsValidPath.path")?;

                    let is_valid =
                        forward_stderr(&mut self.w, self.store.is_valid_path(path)).await?;
                    wire::write_bool(&mut self.w, is_valid)
                        .await
                        .with_field("IsValidPath.is_valid")?;
                }
                Ok(wire::Op::HasSubstitutes) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("HasSubstitutes.path")?;
                    let has_substitutes =
                        forward_stderr(&mut self.w, self.store.has_substitutes(path)).await?;
                    wire::write_bool(&mut self.w, has_substitutes)
                        .await
                        .with_field("HasSubstitutes.has_substitutes")?;
                }
                Ok(wire::Op::AddToStore) => match self.proto {
                    Proto(1, 25..) => {
                        let name = wire::read_string(&mut self.r)
                            .await
                            .with_field("AddToStore.name")?;
                        let cam_str = wire::read_string(&mut self.r)
                            .await
                            .with_field("AddToStore.camStr")?;
                        let refs = wire::read_strings(&mut self.r)
                            .collect::<Result<Vec<_>>>()
                            .await
                            .with_field("AddToStore.refs")?;
                        let repair = wire::read_bool(&mut self.r)
                            .await
                            .with_field("AddToStore.repair")?;
                        let mut source = wire::FramedReader::new(&mut self.r);

                        let (name, pi) = forward_stderr(
                            &mut self.w,
                            self.store
                                .add_to_store(name, cam_str, refs, repair, &mut source),
                        )
                        .await?;

                        // Ensure source is drained to avoid stream desynchronization
                        let mut sink = tokio::io::sink();
                        tokio::io::copy(&mut source, &mut sink)
                            .await
                            .map_err(|e| Error::IO(e))?;

                        wire::write_string(&mut self.w, name)
                            .await
                            .with_field("AddToStore.name")?;
                        wire::write_pathinfo(&mut self.w, self.proto, &pi)
                            .await
                            .with_field("AddToStore.pi")?;
                    }
                    _ => {
                        return Err(Error::Invalid(format!(
                            "AddToStore is not implemented for Protocol {}",
                            self.proto
                        ))
                        .into());
                    }
                },
                Ok(wire::Op::BuildPaths) => {
                    let paths = wire::read_strings(&mut self.r)
                        .collect::<Result<Vec<_>>>()
                        .await
                        .with_field("BuildPaths.paths")?;
                    let mode = if self.proto >= Proto(1, 15) {
                        wire::read_build_mode(&mut self.r)
                            .await
                            .with_field("BuildPaths.build_mode")?
                    } else {
                        BuildMode::Normal
                    };

                    forward_stderr(&mut self.w, self.store.build_paths(paths, mode)).await?;
                    wire::write_u64(&mut self.w, 1)
                        .await
                        .with_field("BuildPaths.__unused__")?;
                }
                Ok(wire::Op::EnsurePath) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("EnsurePath.path")?;
                    forward_stderr(&mut self.w, self.store.ensure_path(path)).await?;
                    wire::write_u64(&mut self.w, 1)
                        .await
                        .with_field("EnsurePath.__unused__")?;
                }
                Ok(wire::Op::AddTempRoot) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("AddTempRoot.path")?;

                    forward_stderr(&mut self.w, self.store.add_temp_root(path)).await?;
                    wire::write_u64(&mut self.w, 1)
                        .await
                        .with_field("AddTempRoot.__unused__")?;
                }
                Ok(wire::Op::AddIndirectRoot) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("AddIndirectRoot.path")?;

                    forward_stderr(&mut self.w, self.store.add_indirect_root(path)).await?;
                    wire::write_u64(&mut self.w, 1)
                        .await
                        .with_field("AddIndirectRoot.__unused__")?;
                }
                Ok(wire::Op::FindRoots) => {
                    let roots = forward_stderr(&mut self.w, self.store.find_roots()).await?;

                    wire::write_u64(&mut self.w, roots.len() as u64)
                        .await
                        .with_field("FindRoots.roots[].<count>")?;
                    for (link, target) in roots {
                        wire::write_string(&mut self.w, link)
                            .await
                            .with_field("FindRoots.roots[].link")?;
                        wire::write_string(&mut self.w, target)
                            .await
                            .with_field("FindRoots.roots[].target")?;
                    }
                }
                Ok(wire::Op::SetOptions) => {
                    let ops = wire::read_client_settings(&mut self.r, self.proto)
                        .await
                        .with_field("SetOptions.clientSettings")?;
                    forward_stderr(&mut self.w, self.store.set_options(ops)).await?;
                }
                Ok(wire::Op::QueryPathInfo) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("QueryPathInfo.path")?;

                    let pi = forward_stderr(&mut self.w, self.store.query_pathinfo(path)).await?;

                    wire::write_bool(&mut self.w, pi.is_some())
                        .await
                        .with_field("QueryPathInfo.is_valid")?;
                    if let Some(pi) = pi {
                        wire::write_pathinfo(&mut self.w, self.proto, &pi)
                            .await
                            .with_field("QueryPathInfo.path_info")?;
                    }
                }
                Ok(wire::Op::QueryValidPaths) => match self.proto {
                    Proto(1, 12..) => {
                        let paths = wire::read_strings(&mut self.r)
                            .collect::<Result<Vec<_>>>()
                            .await
                            .with_field("QueryValidPaths.path")?;
                        let use_substituters = if self.proto >= Proto(1, 27) {
                            wire::read_bool(&mut self.r)
                                .await
                                .with_field("QueryValidPaths.use_substituters")?
                        } else {
                            true
                        };

                        let valid_paths = forward_stderr(
                            &mut self.w,
                            self.store.query_valid_paths(paths, use_substituters),
                        )
                        .await?;

                        wire::write_strings(&mut self.w, valid_paths)
                            .await
                            .with_field("QueryValidPaths.valid_path")?;
                    }
                    _ => {
                        return Err(Error::Invalid(format!(
                            "QueryValidPaths is not implemented for Protocol {}",
                            self.proto
                        ))
                        .into());
                    }
                },
                Ok(wire::Op::QuerySubstitutablePaths) => {
                    let paths = wire::read_strings(&mut self.r)
                        .collect::<Result<Vec<_>>>()
                        .await
                        .with_field("QuerySubstitutablePaths.paths")?;
                    let sub_paths =
                        forward_stderr(&mut self.w, self.store.query_substitutable_paths(paths))
                            .await?;
                    wire::write_strings(&mut self.w, sub_paths)
                        .await
                        .with_field("QuerySubstitutablePaths.sub_paths")?;
                }
                Ok(wire::Op::QueryMissing) => {
                    let paths = wire::read_strings(&mut self.r)
                        .collect::<Result<Vec<_>>>()
                        .await
                        .with_field("QueryMissing.paths")?;

                    let crate::Missing {
                        will_build,
                        will_substitute,
                        unknown,
                        download_size,
                        nar_size,
                    } = forward_stderr(&mut self.w, self.store.query_missing(paths)).await?;

                    wire::write_strings(&mut self.w, will_build)
                        .await
                        .with_field("QueryMissing.will_build")?;
                    wire::write_strings(&mut self.w, will_substitute)
                        .await
                        .with_field("QueryMissing.will_substitute")?;
                    wire::write_strings(&mut self.w, unknown)
                        .await
                        .with_field("QueryMissing.unknown")?;
                    wire::write_u64(&mut self.w, download_size)
                        .await
                        .with_field("QueryMissing.download_size")?;
                    wire::write_u64(&mut self.w, nar_size)
                        .await
                        .with_field("QueryMissing.nar_size")?;
                }
                Ok(wire::Op::QueryValidDerivers) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("QueryValidDerivers.path")?;

                    let derivers =
                        forward_stderr(&mut self.w, self.store.query_valid_derivers(path)).await?;
                    wire::write_strings(&mut self.w, derivers)
                        .await
                        .with_field("QueryValidDerivers.paths")?
                }
                Ok(wire::Op::QueryDerivationOutputMap) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("QueryDerivationOutputMap.paths")?;

                    let outputs =
                        forward_stderr(&mut self.w, self.store.query_derivation_output_map(path))
                            .await?;
                    wire::write_u64(&mut self.w, outputs.len() as u64)
                        .await
                        .with_field("QueryDerivationOutputMap.outputs[].<count>")?;
                    for (name, path) in outputs {
                        wire::write_string(&mut self.w, name)
                            .await
                            .with_field("QueryDerivationOutputMap.outputs[].name")?;
                        wire::write_string(&mut self.w, path)
                            .await
                            .with_field("QueryDerivationOutputMap.outputs[].path")?;
                    }
                }
                Ok(wire::Op::BuildPathsWithResults) => {
                    let paths = wire::read_strings(&mut self.r)
                        .collect::<Result<Vec<_>>>()
                        .await
                        .with_field("BuildPathsWithResults.paths")?;
                    let mode = wire::read_build_mode(&mut self.r)
                        .await
                        .with_field("BuildPathsWithResults.build_mode")?;

                    let results = forward_stderr(
                        &mut self.w,
                        self.store.build_paths_with_results(paths, mode),
                    )
                    .await?;

                    wire::write_u64(&mut self.w, results.len() as u64)
                        .await
                        .with_field("BuildPathsWithResults.results.<count>")?;
                    for (path, result) in results {
                        wire::write_string(&mut self.w, path)
                            .await
                            .with_field("BuildPathsWithResults.results[].path")?;
                        wire::write_build_result(&mut self.w, &result, self.proto)
                            .await
                            .with_field("BuildPathsWithResults.results[].result")?;
                    }
                }
                Ok(v) => todo!("{:#?}", v),

                Err(Error::IO(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    info!("Client disconnected");
                    return Ok(());
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
}

async fn forward_stderr<W: AsyncWriteExt + Unpin, P: Progress>(
    w: &mut W,
    mut prog: P,
) -> Result<P::T, P::Error> {
    while let Some(stderr) = prog.next().await? {
        wire::write_stderr(w, Some(stderr)).await?;
    }
    wire::write_stderr(w, None).await?;
    prog.result().await
}

#[cfg(test)]
mod tests {
    use super::*;

    // Sanity check for version comparisons.
    #[test]
    fn test_version_ord() {
        assert!(Proto(0, 1) > Proto(0, 0));
        assert!(Proto(1, 0) > Proto(0, 0));
        assert!(Proto(1, 0) > Proto(0, 1));
        assert!(Proto(1, 1) > Proto(1, 0));
    }
}
