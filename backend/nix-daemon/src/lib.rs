// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

//! nix-daemon
//! ==========
//!
//! This library exposes an interface for directly talking to a [Nix](https://nixos.org/)
//! daemon.
//!
//! - To connect to a `nix-daemon`, use [`nix::DaemonStore`] (through the [`Store`] trait).
//! - To build your own store, and make it compatible with existing tools (eg. `nix-build`),
//!   implement the [`Store`] trait and use [`nix::DaemonProtocolAdapter`].
//!
//! The [`Store`] protocol mirrors the interface of the latest protocol we support, and
//! may receive breaking changes to keep up.
//!
//! However, as the Nix Daemon protocol is forward compatible, and will negotiate the
//! highest protocol version supported by both ends at connection time, there's no
//! pressure to upgrade; unless compatibility is broken upstream, an old version of this
//! crate should in theory be able to talk to newer Nix until the end of time.
//!
//! The [`nix`] module currently supports Protocol 1.35, and Nix 2.15+. Support for older
//! versions will be added in the future (in particular, Protocol 1.21, used by Nix 2.3).

pub mod nix;

use chrono::{DateTime, Utc};
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::{collections::{HashMap, HashSet}, future::Future};
use strum::{AsRefStr, IntoStaticStr};
use thiserror::Error;
use tokio::io::AsyncReadExt;

pub type Result<T, E = Error> = std::result::Result<T, E>;

trait ResultExt<T, E> {
    fn with_field(self, f: &'static str) -> Result<T>;
}

impl<T, E: Into<Error>> ResultExt<T, E> for Result<T, E> {
    fn with_field(self, f: &'static str) -> Result<T> {
        self.map_err(|err| Error::Field(f, Box::new(err.into())))
    }
}

/// Error enum for the library.
#[derive(Debug, Error)]
pub enum Error {
    /// This error was encountered while reading/writing a specific field.
    #[error("`{0}`: {1}")]
    Field(&'static str, #[source] Box<Error>),
    /// An invalid value of some sort was encountered.
    #[error("invalid value: {0}")]
    Invalid(String),

    /// Error returned from the nix daemon.
    #[error("{0}")]
    NixError(NixError),

    /// IO error.
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

/// A thrown exception from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NixError {
    pub level: Verbosity,
    pub msg: String,
    pub traces: Vec<String>,
}

impl std::fmt::Display for NixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {}", self.level, self.msg)?;
        for trace in self.traces.iter() {
            write!(f, "\n\t{}", trace)?;
        }
        Ok(())
    }
}

/// Real-time logging data returned from a [`Progress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stderr {
    /// A plain line of stderr output.
    Next(String),
    /// An error propagated from Nix.
    Error(NixError),
    /// An activity (such as a build) was started.
    StartActivity(StderrStartActivity),
    /// An activity (such as a build) finished.
    StopActivity { act_id: u64 },
    /// A progress update from an activity.
    Result(StderrResult),
}

/// Type of a [`Stderr::StartActivity`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    AsRefStr,
    IntoStaticStr,
    TryFromPrimitive,
    IntoPrimitive,
)]
#[repr(u64)]
pub enum StderrActivityType {
    Unknown = 0,
    CopyPath = 100,
    FileTransfer = 101,
    Realise = 102,
    CopyPaths = 103,
    Builds = 104,
    Build = 105,
    OptimiseStore = 106,
    VerifyPaths = 107,
    Substitute = 108,
    QueryPathInfo = 109,
    PostBuildHook = 110,
    BuildWaiting = 111,
}
impl From<TryFromPrimitiveError<StderrActivityType>> for Error {
    fn from(value: TryFromPrimitiveError<StderrActivityType>) -> Self {
        Self::Invalid(format!("StderrActivityType({:x})", value.number))
    }
}

/// Notification that an Activity (such as a build) has started.
///
/// TODO: Users should not have to fish data out of the .kind-specific .fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StderrStartActivity {
    /// Activity ID. The same act_id is passed in [`Stderr::StopActivity`] and [`Stderr::Result`].
    pub act_id: u64,
    /// Log level of this activity.
    pub level: Verbosity,
    /// Type of the activity.
    pub kind: StderrActivityType,
    /// Log message.
    pub s: String,
    /// Additional fields. The meaning of these depend on the value of .kind.
    /// TODO: This will be replaced with something more user-friendly in the future.
    pub fields: Vec<StderrField>,
    /// Parent activity, or 0 if this is the top-level one
    pub parent_id: u64,
}

/// Type of a [`StderrResult`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    AsRefStr,
    IntoStaticStr,
    TryFromPrimitive,
    IntoPrimitive,
)]
#[repr(u64)]
pub enum StderrResultType {
    FileLinked = 100,
    BuildLogLine = 101,
    UntrustedPath = 102,
    CorruptedPath = 103,
    SetPhase = 104,
    Progress = 105,
    SetExpected = 106,
    PostBuildLogLine = 107,
}
impl From<TryFromPrimitiveError<StderrResultType>> for Error {
    fn from(value: TryFromPrimitiveError<StderrResultType>) -> Self {
        Self::Invalid(format!("StderrResultType({:x})", value.number))
    }
}

/// Notification that a result of some kind (see [`StderrResultType`]) has been produced.
///
/// TODO: Users should not have to fish data out of the .kind-specific .fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StderrResult {
    /// Activity ID. The same act_id is passed in [`Stderr::StartActivity`] and [`Stderr::StopActivity`].
    pub act_id: u64,
    /// Type of the activity.
    pub kind: StderrResultType,
    /// Additional fields. The meaning of these depend on the value of .kind.
    /// TODO: This will be replaced with something more user-friendly in the future.
    pub fields: Vec<StderrField>,
}

/// A raw field used in [`StderrStartActivity`] and [`StderrResult`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(untagged)]
#[error("{:?}", self)]
pub enum StderrField {
    Int(u64),
    String(String),
}

impl StderrField {
    /// If this is a Self::Int, return the value, else None.
    pub fn as_int(&self) -> Option<&u64> {
        if let Self::Int(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn as_activity_type(&self) -> Option<StderrActivityType> {
        self.as_int().and_then(|v| (*v).try_into().ok())
    }

    /// If this is a Self::String, return the value, else None.
    pub fn as_string(&self) -> Option<&String> {
        if let Self::String(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

impl From<u64> for StderrField {
    fn from(value: u64) -> Self {
        Self::Int(value)
    }
}
impl From<StderrActivityType> for StderrField {
    fn from(value: StderrActivityType) -> Self {
        Self::Int(value.into())
    }
}
impl From<String> for StderrField {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl TryFrom<StderrField> for u64 {
    type Error = StderrField;
    fn try_from(f: StderrField) -> Result<u64, Self::Error> {
        match f {
            StderrField::Int(v) => Ok(v),
            _ => Err(f),
        }
    }
}
impl TryFrom<StderrField> for StderrActivityType {
    type Error = StderrField;
    fn try_from(f: StderrField) -> Result<StderrActivityType, Self::Error> {
        match f {
            StderrField::Int(v) => v.try_into().map_err(|_| f),
            _ => Err(f),
        }
    }
}
impl TryFrom<StderrField> for String {
    type Error = StderrField;
    fn try_from(f: StderrField) -> Result<String, Self::Error> {
        match f {
            StderrField::String(v) => Ok(v),
            _ => Err(f),
        }
    }
}

/// Verbosity of a [`Stderr`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    AsRefStr,
    IntoStaticStr,
    TryFromPrimitive,
    IntoPrimitive,
)]
#[repr(u64)]
pub enum Verbosity {
    Error = 0,
    Warn,
    Notice,
    Info,
    Talkative,
    Chatty,
    Debug,
    Vomit,
}
impl From<TryFromPrimitiveError<Verbosity>> for Error {
    fn from(value: TryFromPrimitiveError<Verbosity>) -> Self {
        Self::Invalid(format!("Verbosity({:x})", value.number))
    }
}

/// Passed to [`Store::build_paths()`] and [`Store::build_paths_with_results()`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    AsRefStr,
    IntoStaticStr,
    TryFromPrimitive,
    IntoPrimitive,
)]
#[repr(u64)]
pub enum BuildMode {
    Normal,
    Repair,
    Check,
}
impl From<TryFromPrimitiveError<BuildMode>> for Error {
    fn from(value: TryFromPrimitiveError<BuildMode>) -> Self {
        Self::Invalid(format!("BuildMode({:x})", value.number))
    }
}

/// Status code for a [`BuildResult`], returned from [`Store::build_paths_with_results()`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    AsRefStr,
    IntoStaticStr,
    TryFromPrimitive,
    IntoPrimitive,
)]
#[repr(u64)]
pub enum BuildResultStatus {
    Built = 0,
    Substituted = 1,
    AlreadyValid = 2,
    PermanentFailure = 3,
    InputRejected = 4,
    OutputRejected = 5,
    /// "possibly transient", the CppNix source helpfully points out.
    TransientFailure = 6,
    /// No longer used, according to a comment in CppNix 2.19.3.
    /// TODO: Figure out since when.
    CachedFailure = 7,
    TimedOut = 8,
    MiscFailure = 9,
    DependencyFailed = 10,
    LogLimitExceeded = 11,
    NotDeterministic = 12,
    ResolvesToAlreadyValid = 13,
    NoSubstituters = 14,
}
impl From<TryFromPrimitiveError<BuildResultStatus>> for Error {
    fn from(value: TryFromPrimitiveError<BuildResultStatus>) -> Self {
        Self::Invalid(format!("BuildResultStatus({:x})", value.number))
    }
}

/// Returned from [`Store::build_paths_with_results()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildResult {
    /// Status code, see BuildResultStatus.
    pub status: BuildResultStatus,
    /// Verbatim error message, or "" if none.
    pub error_msg: String,
    /// How many times this derivation was built. Only present on Proto 1.29+.
    pub times_built: u64, // FIXME: Make Option<>.
    pub is_non_deterministic: bool, // FIXME: Make Option<>.
    pub start_time: DateTime<Utc>,  // FIXME: Make Option<>.
    pub stop_time: DateTime<Utc>,   // FIXME: Make Option<>.
    /// Cumulative CPU time used by this build, in seconds. Only present on Proto 1.37+.
    pub cpu_user: Option<u64>,
    pub cpu_system: Option<u64>,
    /// Map of (name, path). Only present on Proto 1.28+.
    pub built_outputs: HashMap<String, Realisation>, // FIXME: Make Option<>.
}

/// Output name for derivations.
pub type OutputName = String;

/// Store path type.
pub type StorePath = String;

/// Hash digest type.
pub type HashDigest = String;

/// Content address method with algorithm.
pub type ContentAddressMethodWithAlgo = String;

/// Result of building a derivation, returned from [`Store::build_derivation()`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Realisation {
    /// The ID of the derivation that was realised.
    pub id: String,
    /// The path that was realised.
    #[serde(rename = "outPath")]
    pub out_path: StorePath,
    /// The signatures associated with the realised path.
    pub signatures: Vec<String>,
    /// The dependent realisations.
    #[serde(rename = "dependentRealisations")]
    pub dependent_realisations: HashMap<String, StorePath>,
}

/// Output specification for a derivation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationOutput {
    /// Path where the output should be stored (optional).
    pub path: Option<StorePath>,
    /// Content address method with algorithm (optional).
    pub hash_algo: Option<ContentAddressMethodWithAlgo>,
    /// Expected hash digest (optional).
    pub hash: Option<HashDigest>,
}

/// Basic derivation structure for building.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicDerivation {
    /// Map of output names to their specifications.
    pub outputs: HashMap<OutputName, DerivationOutput>,
    /// Set of input source paths.
    pub input_srcs: HashSet<StorePath>,
    /// Target platform for the build.
    pub platform: String,
    /// Builder program to execute.
    pub builder: String,
    /// Arguments to pass to the builder.
    pub args: Vec<String>,
    /// Environment variables for the build.
    pub env: HashMap<String, String>,
    /// Optional structured attributes for advanced build configuration.
    pub structured_attrs: Option<HashMap<String, String>>,
}

/// Passed to [`Store::set_options()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSettings {
    /// Whether to keep temporary directories of failed builds.
    ///
    /// Default: `false`
    pub keep_failed: bool,

    /// Whether to keep building derivations when another build fails.
    ///
    /// Default: `false`
    pub keep_going: bool,

    /// Whether to fall back to building from source if a binary substitution fails.
    ///
    /// Default: `false`
    pub try_fallback: bool,

    /// Verbosity.
    ///
    /// Default: [`Verbosity::Error`].
    pub verbosity: Verbosity,

    /// Number of derivations Nix will attempt to build in parallel.
    ///
    /// 0 = No local builds will be performed (except `preferLocalBuild` derivations),
    ///     only remote builds and substitutions.
    ///
    /// This is different from [`ClientSettings::build_cores`], which affects how many
    /// cores are used by a single build. In `nix.conf`, this can also have the magic
    /// value "auto", which uses the number of threads available on the machine, but we
    /// can only set it to whole numbers using [`Store::set_options`].
    ///
    /// Default: `1`
    pub max_build_jobs: u64,

    /// Number of seconds a build is allowed to produce no stdout or stderr output.
    ///
    /// Default: `0`
    pub max_silent_time: u64,

    /// Whether to show build log output in real time.
    pub verbose_build: bool,
    /// How many cores will be used for an individual build. Sets the `NIX_BUILD_CORES`
    /// environment variable for builders, which is passed to eg. `make -j`.
    ///
    /// 0 = Use all available cores on the builder machine.
    ///
    /// This is different from [`ClientSettings::max_build_jobs`], which controls ho
    /// many derivations Nix will attempt to build in parallel.
    ///
    /// NOTE: On the daemon, this defaults to the number of threads that were available
    /// when it started, which we of course have no way of knowing, since we don't even
    /// know if we're running on the same machine. Thus, we default to 0 instead, which
    /// in most cases will do the same thing.
    ///
    /// If you know you're on the daemon machine, you can get the available threads with:
    ///
    /// ```
    /// use nix_daemon::ClientSettings;
    ///
    /// ClientSettings {
    ///     build_cores: std::thread::available_parallelism().unwrap().get() as u64,
    ///     ..Default::default()
    /// };
    /// ```
    ///
    /// Default: `0`
    pub build_cores: u64,

    /// Whether to use binary substitutes if available.
    ///
    /// Default: `true`
    pub use_substitutes: bool,

    /// Undocumented. Some additional settings can be set using this field.
    ///
    /// Default: [`HashMap::default()`].
    pub overrides: HashMap<String, String>,
}

impl Default for ClientSettings {
    fn default() -> Self {
        // Defaults taken from CppNix: src/libstore/globals.hh
        Self {
            keep_failed: false,
            keep_going: false,
            try_fallback: false,
            verbosity: Verbosity::Error,
            max_build_jobs: 1,
            max_silent_time: 0,
            verbose_build: true,
            build_cores: 0,
            use_substitutes: true,
            overrides: HashMap::default(),
        }
    }
}

/// PathInfo, like `nix path-info` would return.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathInfo {
    /// The first derivation that produced this path.
    ///
    /// Note that if a second derivation produces the same output, eg. because an input
    /// changed, but not in a way which affects the contents of the output, this will
    /// still point to the first derivation, which may not be in your store (anymore).
    ///
    /// If you want to know which derivations actually in your store can produce a path,
    /// use [`Store::query_valid_derivers()`] instead.
    pub deriver: Option<String>,

    /// Other paths referenced by this path.
    pub references: Vec<String>,

    /// NAR hash, in the form: "(algo)-(hash)".
    pub nar_hash: String,
    /// NAR size.
    pub nar_size: u64,

    /// Is this path "ultimately trusted", eg. built locally?
    pub ultimate: bool,
    /// Optional signatures, eg. from a binary cache.
    pub signatures: Vec<String>,
    /// An assertion that this path is content-addressed, eg. for fixed-output derivations.
    pub ca: Option<String>,

    /// When the path was registered, eg. placed into the local store.
    pub registration_time: DateTime<Utc>,
}

/// An in-progress operation, which may produces a series of [`Stderr`]s before returning.
///
/// All functions on the Store trait return these wrappers. If you just want the final result, not
/// play-by-play updates on eg. log output or paths built:
///
/// ```no_run
/// use nix_daemon::{Store, Progress, nix::DaemonStore};
/// # async {
/// # let mut store = DaemonStore::builder()
/// #     .connect_unix("/nix/var/nix/daemon-socket/socket")
/// #     .await?;
/// #
/// let is_valid_path = store.is_valid_path("/nix/store/...").result().await?;
/// # Ok::<_, nix_daemon::Error>(())
/// # };
/// ```
///
/// Otherwise, if you are interested in (some of) the progress updates:
///
/// ```no_run
/// use nix_daemon::{Store, Progress, nix::DaemonStore};
/// # async {
/// # let mut store = DaemonStore::builder()
/// #     .connect_unix("/nix/var/nix/daemon-socket/socket")
/// #     .await?;
/// #
/// let mut prog = store.is_valid_path("/nix/store/...");
/// while let Some(stderr) = prog.next().await? {
///     match stderr {
///         _ => todo!(),
///     }
/// }
/// let is_valid_path = prog.result().await?;
/// # Ok::<_, nix_daemon::Error>(())
/// # };
/// ```
pub trait Progress: Send {
    type T: Send;
    type Error: From<Error> + Send + Sync;

    /// Returns the next Stderr message, or None after all have been consumed.
    /// This must behave like a fused iterator - once None is returned, all further calls
    /// must immediately return None, without corrupting the underlying datastream, etc.
    fn next(&mut self) -> impl Future<Output = Result<Option<Stderr>, Self::Error>> + Send;

    /// Discards any further messages from [`Self::next()`] and proceeds.
    fn result(self) -> impl Future<Output = Result<Self::T, Self::Error>> + Send;
}

/// Helper methods for [`Progress`].
pub trait ProgressExt: Progress {
    /// Calls `f()` for each message returned from [`Progress::next()`], then [`Progress::result()`].
    fn inspect_each<F: Fn(Stderr) + Send>(
        self,
        f: F,
    ) -> impl Future<Output = Result<Self::T, Self::Error>> + Send;

    /// Returns a tuple of (stderr, [`Progress::result()`]), where stderr is a `Vec<Stderr>` of all
    /// Stderr returned by [`Progress::next()`].
    fn split(self) -> impl Future<Output = (Vec<Stderr>, Result<Self::T, Self::Error>)> + Send;
}
impl<P: Progress> ProgressExt for P {
    async fn inspect_each<F: Fn(Stderr)>(mut self, f: F) -> Result<Self::T, Self::Error> {
        while let Some(stderr) = self.next().await? {
            f(stderr)
        }
        self.result().await
    }

    async fn split(mut self) -> (Vec<Stderr>, Result<Self::T, Self::Error>) {
        let mut stderrs = Vec::new();
        loop {
            match self.next().await {
                Ok(Some(stderr)) => stderrs.push(stderr),
                Err(err) => break (stderrs, Err(err)),
                Ok(None) => break (stderrs, self.result().await),
            }
        }
    }
}

/// Generic interface to a Nix store.
///
/// See [`nix::DaemonStore`] for an implementation that talks to a CppNix (compatible) daemon.
pub trait Store {
    type Error: From<Error> + Send + Sync;

    /// Returns whether a store path is valid.
    fn is_valid_path<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Progress<T = bool, Error = Self::Error>;

    /// Returns whether `Self::query_substitutable_paths()` would return anything.
    fn has_substitutes<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Progress<T = bool, Error = Self::Error>;

    /// Adds a file to the store.
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
        R: AsyncReadExt + Unpin + Send + Debug;

    /// Builds the specified paths.
    fn build_paths<Paths>(
        &mut self,
        paths: Paths,
        mode: BuildMode,
    ) -> impl Progress<T = (), Error = Self::Error>
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync;

    /// Ensure the specified store path exists.
    fn ensure_path<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> impl Progress<T = (), Error = Self::Error>;

    /// Creates a temporary GC root, which persists until the client disconnects.
    fn add_temp_root<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> impl Progress<T = (), Error = Self::Error>;

    /// Creates a persistent GC root. This is what's normally meant by a GC root.
    fn add_indirect_root<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> impl Progress<T = (), Error = Self::Error>;

    /// Returns the `(link, target)` of all known GC roots.
    fn find_roots(&mut self) -> impl Progress<T = HashMap<String, String>, Error = Self::Error>;

    /// Applies client options. This changes the behaviour of future commands.
    fn set_options(&mut self, opts: ClientSettings) -> impl Progress<T = (), Error = Self::Error>;

    /// Returns a [`PathInfo`] struct for the given path.
    fn query_pathinfo<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> impl Progress<T = Option<PathInfo>, Error = Self::Error>;

    /// Returns which of the passed paths are valid.
    fn query_valid_paths<Paths>(
        &mut self,
        paths: Paths,
        use_substituters: bool,
    ) -> impl Progress<T = Vec<String>, Error = Self::Error>
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync;

    /// Returns paths which can be substituted.
    fn query_substitutable_paths<Paths>(
        &mut self,
        paths: Paths,
    ) -> impl Progress<T = Vec<String>, Error = Self::Error>
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync;

    /// Returns a list of valid derivers for a path.
    /// This is sort of like [`PathInfo::deriver`], but it doesn't lie to you.
    fn query_valid_derivers<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> impl Progress<T = Vec<String>, Error = Self::Error>;

    /// Takes a list of paths and queries which would be built, substituted or unknown,
    /// along with an estimate of the cumulative download and NAR sizes.
    fn query_missing<Ps>(&mut self, paths: Ps) -> impl Progress<T = Missing, Error = Self::Error>
    where
        Ps: IntoIterator + Send + Debug,
        Ps::IntoIter: ExactSizeIterator + Send,
        Ps::Item: AsRef<str> + Send + Sync;

    /// Returns a map of `(output, store path)` for the given derivation.
    fn query_derivation_output_map<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Progress<T = HashMap<String, String>, Error = Self::Error>;

    /// Like `Self::build_paths()`, but returns a [`BuildResult`] for each entry in `paths`.
    fn build_paths_with_results<Ps>(
        &mut self,
        paths: Ps,
        mode: BuildMode,
    ) -> impl Progress<T = HashMap<String, BuildResult>, Error = Self::Error>
    where
        Ps: IntoIterator + Send + Debug,
        Ps::IntoIter: ExactSizeIterator + Send,
        Ps::Item: AsRef<str> + Send + Sync;

    /// Build a single derivation.
    fn build_derivation<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        drv_path: P,
        drv: BasicDerivation,
        build_mode: BuildMode,
    ) -> impl Progress<T = BuildResult, Error = Self::Error>;

    /// Export a store path as NAR data (equivalent to C++ narFromPath).
    fn nar_from_path<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Progress<T = Vec<String>, Error = Self::Error>;

    /// Add a path to the store with ValidPathInfo and NAR data (equivalent to C++ addToStore with info and source).
    fn add_to_store_nar<P: AsRef<str> + Send + Sync + Debug, R>(
        &mut self,
        path: P,
        path_info: PathInfo,
        nar_source: R,
    ) -> impl Progress<T = (), Error = Self::Error>
    where
        R: IntoIterator + Send + Debug,
        R::IntoIter: ExactSizeIterator + Send,
        R::Item: AsRef<str> + Send + Sync;
}

/// Returned from [`Store::query_missing()`].
#[derive(Debug, PartialEq, Eq)]
pub struct Missing {
    /// Paths that will be built.
    pub will_build: Vec<String>,
    /// Paths that will be substituted.
    pub will_substitute: Vec<String>,
    /// Paths we don't know what will happen to.
    pub unknown: Vec<String>,
    /// Despite the name, the extracted size of all substituted paths.
    pub download_size: u64,
    /// Total size of all NARs to download from a substituter.
    pub nar_size: u64,
}
