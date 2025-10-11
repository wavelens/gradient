// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
// SPDX-FileCopyrightText: 2024 Erin Shepherd <erin.shepherd@e43.eu>
//
// SPDX-License-Identifier: EUPL-1.2

//! Low-level helpers for the nix-daemon wire format.

use crate::{
    BuildMode, BuildResult, BuildResultStatus, ClientSettings, Error, NixError, PathInfo, Result,
    ResultExt, Stderr, StderrField, StderrResult, StderrStartActivity, Verbosity, nix::Proto,
};
use async_stream::try_stream;
use chrono::{DateTime, Utc};
use futures::future::OptionFuture;
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use std::collections::HashMap;
use std::fmt::Debug;
use std::task::Poll;
use tap::{Tap, TapFallible};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};
use tokio_stream::{Stream, StreamExt};
use tracing::{instrument, trace};

/// Magic number sent by the client.
pub const WORKER_MAGIC_1: u64 = 0x6e697863;
/// Magic number sent by the daemon.
pub const WORKER_MAGIC_2: u64 = 0x6478696f;

/// Opcodes.
///
/// Not included are Ops that were obsolete before the earliest Nix version we support:
///
/// - Nix 2.0 (2016-04-19: e0204f8d462041387651af388074491fd0bf36d6)
///   - QueryPathHash = 4
///   - QueryReferences = 5
///   - QueryDeriver = 18
/// - Nix 2.0 (2016-05-04: 538a64e8c314f23ba0c5d76201f1c20e71884a21)
///   - ExportPath = 16
///   - ImportPaths = 27
#[derive(Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum Op {
    IsValidPath = 1,
    HasSubstitutes = 3,
    QueryReferrers = 6,
    AddToStore = 7,
    BuildPaths = 9,
    EnsurePath = 10,
    AddTempRoot = 11,
    AddIndirectRoot = 12,
    SyncWithGC = 13,
    FindRoots = 14,
    SetOptions = 19,
    CollectGarbage = 20, // TODO: Can't be safely cargo tested on host daemon+store.
    QuerySubstitutablePathInfo = 21,
    QueryAllValidPaths = 23,
    QueryFailedPaths = 24,
    ClearFailedPaths = 25,
    QueryPathInfo = 26,
    QueryPathFromHashPart = 29,
    QuerySubstitutablePathInfos = 30,
    QueryValidPaths = 31,
    QuerySubstitutablePaths = 32,
    QueryValidDerivers = 33,
    OptimiseStore = 34, // TODO: Can't be safely cargo tested on host daemon+store.
    VerifyStore = 35,   // TODO: Can't be safely cargo tested on host daemon+store.
    BuildDerivation = 36,
    AddSignatures = 37,
    NarFromPath = 38,
    AddToStoreNar = 39,
    QueryMissing = 40,
    QueryDerivationOutputMap = 41,
    RegisterDrvOutput = 42,
    QueryRealisation = 43,
    AddMultipleToStore = 44,
    AddBuildLog = 45,
    BuildPathsWithResults = 46,

    /// Obsolete since Nix 2.4, use AddToStore.
    /// <https://github.com/NixOS/nix/commit/c602ebfb34de3626fa0b9110face6ea4b171ac0f>
    AddTextToStore = 8,
    /// Obsolete since Nix 2.4, use QueryDerivationOutputMap.
    /// <https://github.com/NixOS/nix/commit/d38f860c3ef001a456d4d447f89219de5e3c830c>
    QueryDerivationOutputs = 22,
    /// Obsolete since Nix 2.4, get it from any derivation struct.
    /// <https://github.com/NixOS/nix/commit/045b07200c77bf1fe19c0a986aafb531e7e1ba54>
    QueryDerivationOutputNames = 28,
}
impl From<TryFromPrimitiveError<Op>> for Error {
    fn from(value: TryFromPrimitiveError<Op>) -> Self {
        Self::Invalid(format!("Op({:x})", value.number))
    }
}

/// Reader compatible with CppNix' FramedSource/FramedSink protocol.
///
/// Each "frame" is a u64 length, followed by that number of bytes.
/// The stream is terminated by a frame of length 0.
#[derive(Debug)]
pub struct FramedReader<'r, R: AsyncReadExt + Unpin> {
    r: std::pin::Pin<&'r mut R>,
    header: [u8; 8],
    header_read: u8,
    done: bool,
}

impl<'r, R: AsyncReadExt + Unpin> FramedReader<'r, R> {
    pub fn new(r: &'r mut R) -> Self {
        Self {
            r: std::pin::Pin::new(r),
            header: 0u64.to_le_bytes(),
            header_read: 0,
            done: false,
        }
    }

    fn remaining(&self) -> u64 {
        u64::from_le_bytes(self.header)
    }

    fn consume(&mut self, n: u64) {
        self.header = (self.remaining() - n).to_le_bytes();
    }

    fn read_header(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        while self.header_read < 8 {
            let mut stkbuf = self.header;
            let mut buf = ReadBuf::new(&mut stkbuf[self.header_read as usize..]);
            self.header_read += match self.r.as_mut().poll_read(cx, &mut buf) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {
                    let n = buf.filled().len();
                    self.header = stkbuf;
                    if n == 0 {
                        return Poll::Ready(Err(std::io::ErrorKind::UnexpectedEof.into()));
                    }
                    n as u8
                }
            };
        }
        self.header_read = 0;
        self.done = self.remaining() == 0;
        Poll::Ready(Ok(()))
    }
}

impl<'r, R: AsyncReadExt + Unpin> AsyncRead for FramedReader<'r, R> {
    #[instrument(name = "FramedReader", skip_all)]
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        while !self.done && self.remaining() == 0 {
            match self.as_mut().read_header(cx) {
                Poll::Ready(Ok(())) => trace!(len = self.remaining(), "Read header"),
                ret => return ret,
            };
        }

        if self.done {
            trace!("Done");
            return Poll::Ready(Ok(()));
        }

        let len_before = buf.filled().len();
        let remaining = self.remaining();
        let r = self.r.as_mut().take(remaining);
        trace!(len_before, remaining, "Reading...");
        match std::pin::pin!(r).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {}
            ret => return ret,
        }

        let read = buf.filled().len() - len_before;
        self.consume(read as u64);
        trace!(len_before, remaining, read, "Read");
        if read == 0 {
            return Poll::Ready(Err(std::io::ErrorKind::UnexpectedEof.into()));
        }
        Poll::Ready(Ok(()))
    }
}

#[instrument(skip_all, level = "trace")]
pub async fn copy_to_framed<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    r: &mut R,
    w: &mut W,
    buf: &mut [u8],
) -> Result<()> {
    loop {
        let len = r.read(buf).await?;
        write_u64(w, len as u64).await?;
        if len == 0 {
            trace!("Done");
            return Ok(());
        }
        w.write_all(&buf[..len]).await?;
        trace!(len, "Copied frame...");
    }
}

/// Read a u64 from the stream (little endian).
#[instrument(skip(r), level = "trace")]
pub async fn read_u64<R: AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<u64> {
    r.read_u64_le().await.tap_ok(|v| trace!(v, "<-"))
}
/// Write a u64 from the stream (little endian).
#[instrument(skip(w, v), level = "trace")]
pub async fn write_u64<W: AsyncWriteExt + Unpin>(w: &mut W, v: u64) -> std::io::Result<()> {
    w.write_u64_le(v.tap(|v| trace!(v, "->"))).await
}

/// Read a boolean from the stream, encoded as u64 (>0 is true).
#[instrument(skip(r), level = "trace")]
pub async fn read_bool<R: AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<bool> {
    read_u64(r)
        .await
        .map(|v| v > 0)
        .tap_ok(|v| trace!(v, "<-"))
}
/// Write a boolean to the stream, encoded as u64 (>0 is true).
#[instrument(skip(w, v), level = "trace")]
pub async fn write_bool<W: AsyncWriteExt + Unpin>(w: &mut W, v: bool) -> std::io::Result<()> {
    Ok(write_u64(w, if v { 1 } else { 0 }.tap(|v| trace!(v, "->"))).await?)
}

/// Read a DateTime (CppNix: time_t) from the stream, encoded as a unix timestamp.
#[instrument(skip(r), level = "trace")]
pub async fn read_datetime<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<DateTime<Utc>> {
    read_u64(r).await.map_err(Into::into).and_then(|ts| {
        DateTime::from_timestamp(ts as i64, 0)
            .ok_or_else(|| Error::Invalid(ts.to_string()))
            .tap_ok(|dt| trace!(?dt, "<-"))
    })
}
/// Write a DateTime (CppNix: time_t) from the stream, encoded as a unix timestamp.
#[instrument(skip(w), level = "trace")]
pub async fn write_datetime<W: AsyncWriteExt + Unpin>(w: &mut W, dt: DateTime<Utc>) -> Result<()> {
    Ok(write_u64(
        w,
        dt.timestamp()
            .tap(|dt| trace!(?dt, "->"))
            .try_into()
            .map_err(|err| Error::Invalid(format!("DateTime({}): {}", dt.to_string(), err)))?,
    )
    .await?)
}

/// Read a protocol version from the stream.
#[instrument(skip(r), level = "trace")]
pub async fn read_proto<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<Proto> {
    Ok(read_u64(r)
        .await
        .map(|raw| raw.into())
        .tap_ok(|v| trace!(?v, "<-"))?)
}
/// Write a protocol version to the stream.
#[instrument(skip(w, v), level = "trace")]
pub async fn write_proto<W: AsyncWriteExt + Unpin>(w: &mut W, v: Proto) -> Result<()> {
    Ok(write_u64(w, v.tap(|v| trace!(?v, "->")).into()).await?)
}

/// Read an opcode from the stream.
#[instrument(skip(r), level = "trace")]
pub async fn read_op<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<Op> {
    Ok(read_u64(r).await?.try_into().tap_ok(|v| trace!(?v, "<-"))?)
}
/// Write an opcode to the stream.
#[instrument(skip(w, v), level = "trace")]
pub async fn write_op<W: AsyncWriteExt + Unpin>(w: &mut W, v: Op) -> Result<()> {
    Ok(write_u64(w, v.tap(|v| trace!(?v, "->")).into()).await?)
}

/// Read a verbosity level from the stream.
#[instrument(skip(r), level = "trace")]
pub async fn read_verbosity<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<Verbosity> {
    Ok(read_u64(r).await?.try_into().tap_ok(|v| trace!(?v, "<-"))?)
}
/// Write a verbosity level to the stream.
#[instrument(skip(w, v), level = "trace")]
pub async fn write_verbosity<W: AsyncWriteExt + Unpin>(w: &mut W, v: Verbosity) -> Result<()> {
    Ok(write_u64(w, v.tap(|v| trace!(?v, "->")).into()).await?)
}

/// Read a build mode from the stream.
#[instrument(skip(r), level = "trace")]
pub async fn read_build_mode<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<BuildMode> {
    Ok(read_u64(r).await?.try_into().tap_ok(|v| trace!(?v, "<-"))?)
}
/// Write a build mode to the stream.
#[instrument(skip(w, v), level = "trace")]
pub async fn write_build_mode<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    v: BuildMode,
) -> std::io::Result<()> {
    write_u64(w, v.tap(|v| trace!(?v, "->")).into()).await
}

/// Read a build result status from the stream.
#[instrument(skip(r), level = "trace")]
pub async fn read_build_result_status<R: AsyncReadExt + Unpin>(
    r: &mut R,
) -> Result<BuildResultStatus> {
    Ok(read_u64(r).await?.try_into().tap_ok(|v| trace!(?v, "<-"))?)
}
/// Write a build result status to the stream.
#[instrument(skip(w, v), level = "trace")]
pub async fn write_build_result_status<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    v: BuildResultStatus,
) -> std::io::Result<()> {
    write_u64(w, v.tap(|v| trace!(?v, "->")).into()).await
}

/// Read a string from the stream. Strings are prefixed with a u64 length, but the
/// data is padded to the next 8-byte boundary, eg. a 1-byte string becomes 16 bytes
/// on the wire: 8 for the length, 1 for the data, then 7 bytes of discarded 0x00s.
#[instrument(skip(r), level = "trace")]
pub async fn read_string<R: AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<String> {
    let len = read_u64(r).await? as usize;
    let padded_len = len + if len % 8 > 0 { 8 - (len % 8) } else { 0 };
    if padded_len <= 1024 {
        let mut buf = [0u8; 1024];
        r.read_exact(&mut buf[..padded_len]).await?;
        Ok(String::from_utf8_lossy(&buf[..len]).to_string())
    } else {
        let mut buf = vec![0u8; padded_len];
        r.read_exact(&mut buf[..padded_len]).await?;
        Ok(String::from_utf8_lossy(&buf[..len]).to_string())
    }
    .tap_ok(|v| trace!(v, "<-"))
}

/// Write a string to the stream. See: NixReader::read_string.
#[instrument(skip(w, s), level = "trace")]
pub async fn write_string<W: AsyncWriteExt + Unpin, S: AsRef<str> + Debug>(
    w: &mut W,
    s: S,
) -> std::io::Result<()> {
    trace!(v=?s,"->");
    let truncated = s.as_ref().split(|b| b == '\0').next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            Error::Invalid("slice::split() returned an empty iterator".to_string()),
        )
    })?;
    let b = truncated.as_bytes();
    write_u64(
        w,
        b.len().try_into().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("String length too large: {}", e),
            )
        })?,
    )
    .await?;
    if b.len() > 0 {
        w.write_all(b).await?;
        trace!(v = truncated, "->");
        if b.len() % 8 > 0 {
            let pad_buf = [0u8; 7];
            let pad_len = 8 - (b.len() % 8);
            w.write_all(&pad_buf[..pad_len]).await?;
            trace!(pad_len, "[ padding ]");
        }
    }
    Ok(())
}

/// Read a list (or set) of strings from the stream - a u64 count, followed by that
/// many strings using the normal `read_string()` encoding.
#[instrument(skip(r), level = "trace")]
pub fn read_strings<R: AsyncReadExt + Unpin>(r: &mut R) -> impl Stream<Item = Result<String>> + '_ {
    try_stream! {
        let count = read_u64(r).await.with_field("<count>")? as usize;
        for _ in 0..count {
            yield read_string(r).await?;
        }
    }
}
/// Write a list of strings to the stream.
#[instrument(skip(w, si), level = "trace")]
pub async fn write_strings<W: AsyncWriteExt + Unpin, I>(w: &mut W, si: I) -> std::io::Result<()>
where
    I: IntoIterator + Send,
    I::IntoIter: ExactSizeIterator + Send,
    I::Item: AsRef<str> + Send + Sync,
{
    let si = si.into_iter();
    write_u64(
        w,
        si.len().try_into().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("String collection length too large: {}", e),
            )
        })?,
    )
    .await?;
    for s in si {
        write_string(w, s.as_ref()).await?;
    }
    Ok(())
}

/// Read a NixError struct from the stream.
#[instrument(skip(r), level = "trace")]
pub async fn read_error<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<NixError> {
    match read_string(r).await {
        Err(err) => Err(err.into()),
        Ok(s) if s.as_str() == "Error" => Ok(()),
        Ok(s) => Err(Error::Invalid(format!("expected 'Error', got '{}'", s))),
    }
    .with_field("Error.__unused_type_1")?;

    let level = read_verbosity(r).await.with_field("Error.level")?;

    match read_string(r).await {
        Err(err) => Err(err.into()),
        Ok(s) if s.as_str() == "Error" => Ok(()),
        Ok(s) => Err(Error::Invalid(format!("expected 'Error', got '{}'", s))),
    }
    .with_field("Error.__unused_type_2")?;

    let msg = read_string(r).await.with_field("Error.msg")?;

    read_u64(r).await.with_field("Error.__unused_err_pos")?;

    let num_traces = read_u64(r).await.with_field("Error.traces[].<count>")?;
    let mut traces = Vec::with_capacity(num_traces.try_into().unwrap_or_default());
    for _ in 0..num_traces {
        read_u64(r)
            .await
            .with_field("Error.traces[].__unused_pos")?;
        traces.push(read_string(r).await.with_field("Error.traces[].hint")?);
    }

    Ok(NixError { level, msg, traces })
}

/// Write a NixError struct to the stream.
#[instrument(skip(w, v), level = "trace")]
pub async fn write_error<W: AsyncWriteExt + Unpin>(w: &mut W, v: NixError) -> Result<()> {
    write_string(w, "Error")
        .await
        .with_field("Error.__unused_type_1")?;

    write_verbosity(w, v.level)
        .await
        .with_field("Error.level")?;

    write_string(w, "Error")
        .await
        .with_field("Error.__unused_type_2")?;

    write_string(w, v.msg).await.with_field("Error.msg")?;

    write_u64(w, 0).await.with_field("Error.__unused_err_pos")?;

    write_u64(w, v.traces.len() as u64)
        .await
        .with_field("Error.traces[].<count>")?;
    for trace in v.traces.iter() {
        write_u64(w, 0)
            .await
            .with_field("Error.traces[].__unused_pos")?;
        write_string(w, trace)
            .await
            .with_field("Error.traces[].hint")?;
    }

    Ok(())
}

#[instrument(skip(r), level = "trace")]
pub async fn read_build_result<R: AsyncReadExt + Unpin>(
    r: &mut R,
    proto: Proto,
) -> Result<BuildResult> {
    let status = read_build_result_status(r)
        .await
        .with_field("BuildResult.status")?;
    let error_msg = read_string(r).await.with_field("BuildResult.error_msg")?;

    let mut br = BuildResult {
        status,
        error_msg,
        times_built: 0,
        is_non_deterministic: false,
        start_time: DateTime::default(),
        stop_time: DateTime::default(),
        built_outputs: HashMap::default(),
    };

    if proto >= Proto(1, 29) {
        br.times_built = read_u64(r).await.with_field("BuildResult.times_built")?;
        br.is_non_deterministic = read_bool(r)
            .await
            .with_field("BuildResult.is_non_deterministic")?;
        br.start_time = read_datetime(r)
            .await
            .with_field("BuildResult.start_time")?;
        br.stop_time = read_datetime(r).await.with_field("BuildResult.stop_time")?;
    }
    if proto >= Proto(1, 28) {
        let count = read_u64(r)
            .await
            .with_field("BuildResult.built_outputs.<count>")? as usize;
        for _ in 0..count {
            let name = read_string(r)
                .await
                .with_field("BuildResult.built_outputs[].name")?;
            let path = read_string(r)
                .await
                .with_field("BuildResult.built_outputs[].path")?;
            br.built_outputs.insert(name, path);
        }
    }

    Ok(br)
}

#[instrument(skip(w), level = "trace")]
pub async fn write_build_result<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    result: &BuildResult,
    proto: Proto,
) -> Result<()> {
    write_build_result_status(w, result.status)
        .await
        .with_field("BuildResult.status")?;
    write_string(w, &result.error_msg)
        .await
        .with_field("BuildResult.error_msg")?;

    if proto >= Proto(1, 29) {
        write_u64(w, result.times_built)
            .await
            .with_field("BuildResult.times_built")?;
        write_bool(w, result.is_non_deterministic)
            .await
            .with_field("BuildResult.is_non_deterministic")?;
        write_datetime(w, result.start_time)
            .await
            .with_field("BuildResult.start_time")?;
        write_datetime(w, result.stop_time)
            .await
            .with_field("BuildResult.stop_time")?;
    }
    if proto >= Proto(1, 28) {
        write_u64(w, result.built_outputs.len() as u64)
            .await
            .with_field("BuildResult.built_outputs.<count>")?;
        for (name, path) in &result.built_outputs {
            write_string(w, name)
                .await
                .with_field("BuildResult.built_outputs[].name")?;
            write_string(w, path)
                .await
                .with_field("BuildResult.built_outputs[].path")?;
        }
    }

    Ok(())
}

#[derive(Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum StderrKind {
    Next = 0x6f6c6d67,
    Last = 0x616c7473,
    Error = 0x63787470,
    StartActivity = 0x53545254,
    StopActivity = 0x53544f50,
    Result = 0x52534c54,
}

#[instrument(skip(r), level = "trace")]
pub async fn read_stderr<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<Option<Stderr>> {
    let kind = StderrKind::try_from(read_u64(r).await?)
        .map_err(|TryFromPrimitiveError { number }| {
            Error::Invalid(format!("Stderr<{:#x}>", number))
        })?
        .tap(|kind| trace!(?kind, "<-"));

    match kind {
        StderrKind::Last => Ok(None),
        StderrKind::Next => Ok(Some(Stderr::Next(read_string(r).await?))),
        StderrKind::Error => Ok(Some(Stderr::Error(read_error(r).await?))),
        StderrKind::StartActivity => Ok(Some(Stderr::StartActivity(
            read_stderr_start_activity(r).await?,
        ))),
        StderrKind::StopActivity => Ok(Some(Stderr::StopActivity {
            act_id: read_u64(r).await?,
        })),
        StderrKind::Result => Ok(Some(Stderr::Result(read_stderr_result(r).await?))),
    }
    .tap_ok(|stderr| trace!(?stderr, "<-"))
}
#[instrument(skip(r), level = "trace")]
pub async fn read_stderr_start_activity<R: AsyncReadExt + Unpin>(
    r: &mut R,
) -> Result<StderrStartActivity> {
    Ok(StderrStartActivity {
        act_id: read_u64(r).await?,
        level: read_verbosity(r).await?,
        kind: read_u64(r).await?.try_into()?,
        s: read_string(r).await?,
        fields: read_stderr_fields(r).await?,
        parent_id: read_u64(r).await?,
    }
    .tap(|act| trace!(?act, "<-")))
}
#[instrument(skip(r), level = "trace")]
pub async fn read_stderr_result<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<StderrResult> {
    Ok(StderrResult {
        act_id: read_u64(r).await?,
        kind: read_u64(r).await?.try_into()?,
        fields: read_stderr_fields(r).await?,
    }
    .tap(|res| trace!(?res, "<-")))
}
#[instrument(skip(r), level = "trace")]
pub async fn read_stderr_fields<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<Vec<StderrField>> {
    let count = read_u64(r)
        .await
        .with_field("StartActivity.fields.<count>")?
        .tap(|count| trace!(count, "fields[].<count>")) as usize;
    let mut fields = Vec::with_capacity(count);
    for n in 0..count {
        fields.push(
            match read_u64(r)
                .await
                .with_field("StartActivity.fields[].<type>")?
            {
                0 => Ok(StderrField::Int(read_u64(r).await?)),
                1 => Ok(StderrField::String(read_string(r).await?)),
                v => Err(Error::Invalid(format!("<type>({})", v))),
            }
            .with_field("StartActivity.fields[]")?
            .tap(|v| trace!(n, count, ?v, "fields[]")),
        )
    }
    Ok(fields)
}

#[instrument(skip(w), level = "trace")]
pub async fn write_stderr<W: AsyncWriteExt + Unpin>(w: &mut W, v: Option<Stderr>) -> Result<()> {
    trace!(?v, "->");
    match v {
        None => write_u64(w, StderrKind::Last.into()).await?,
        Some(Stderr::Next(s)) => {
            write_u64(w, StderrKind::Next.into()).await?;
            write_string(w, s).await?;
        }
        Some(Stderr::Error(err)) => {
            write_u64(w, StderrKind::Error.into()).await?;
            write_error(w, err).await?;
        }
        Some(Stderr::StartActivity(start)) => {
            write_u64(w, StderrKind::StartActivity.into()).await?;
            write_stderr_start_activity(w, start).await?;
        }
        Some(Stderr::StopActivity { act_id }) => {
            write_u64(w, StderrKind::StopActivity.into()).await?;
            write_u64(w, act_id).await?;
        }
        Some(Stderr::Result(res)) => {
            write_u64(w, StderrKind::Result.into()).await?;
            write_stderr_result(w, res).await?;
        }
    }
    Ok(())
}
#[instrument(skip(w, v), level = "trace")]
pub async fn write_stderr_start_activity<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    v: StderrStartActivity,
) -> Result<()> {
    trace!(?v, "->");
    write_u64(w, v.act_id).await?;
    write_verbosity(w, v.level).await?;
    write_u64(w, v.kind.into()).await?;
    write_string(w, v.s).await?;
    write_stderr_fields(w, v.fields).await?;
    write_u64(w, v.parent_id).await?;
    Ok(())
}
#[instrument(skip(w, v), level = "trace")]
pub async fn write_stderr_result<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    v: StderrResult,
) -> Result<()> {
    trace!(?v, "->");
    write_u64(w, v.act_id).await?;
    write_u64(w, v.kind.into()).await?;
    write_stderr_fields(w, v.fields).await?;
    Ok(())
}
#[instrument(skip(w, vs), level = "trace")]
pub async fn write_stderr_fields<W: AsyncWriteExt + Unpin, I>(w: &mut W, vs: I) -> Result<()>
where
    I: IntoIterator + Send,
    I::IntoIter: ExactSizeIterator<Item = StderrField> + Send,
{
    let vs = vs.into_iter();
    write_u64(w, vs.len() as u64)
        .await
        .with_field("StartActivity.fields.<count>")?;
    for field in vs {
        match field {
            StderrField::Int(v) => {
                write_u64(w, 0)
                    .await
                    .with_field("StartActivity.fields[].<type>")?;
                write_u64(w, v).await.with_field("StartActivity.fields[]")?;
            }
            StderrField::String(v) => {
                write_u64(w, 0)
                    .await
                    .with_field("StartActivity.fields[].<type>")?;
                write_string(w, v)
                    .await
                    .with_field("StartActivity.fields[]")?;
            }
        }
    }
    Ok(())
}

/// Read a ClientSettings structure from the stream.
#[instrument(skip(r), level = "trace")]
pub async fn read_client_settings<R: AsyncReadExt + Unpin>(
    r: &mut R,
    proto: Proto,
) -> Result<ClientSettings> {
    let keep_failed = read_bool(r)
        .await
        .with_field("ClientSettings.keep_failed")?;
    let keep_going = read_bool(r).await.with_field("ClientSettings.keep_going")?;
    let try_fallback = read_bool(r)
        .await
        .with_field("ClientSettings.try_fallback")?;
    let verbosity = read_verbosity(r)
        .await
        .with_field("ClientSettings.verbosity")?;
    let max_build_jobs = read_u64(r)
        .await
        .with_field("ClientSettings.max_build_jobs")?;
    let max_silent_time = read_u64(r)
        .await
        .with_field("ClientSettings.max_silent_time")?;
    read_u64(r)
        .await
        .with_field("ClientSettings.__obsolete_use_build_hook")?;
    let verbose_build = read_verbosity(r)
        .await
        .map(|v| v == Verbosity::Error)
        .with_field("ClientSettings.verbose_build")?;
    read_u64(r)
        .await
        .with_field("ClientSettings.__obsolete_log_type")?;
    read_u64(r)
        .await
        .with_field("ClientSettings.__obsolete_print_build_trace")?;
    let build_cores = read_u64(r).await.with_field("ClientSettings.build_cores")?;
    let use_substitutes = read_bool(r)
        .await
        .with_field("ClientSettings.use_substitutes")?;

    let overrides = if proto >= Proto(1, 12) {
        let count = read_u64(r)
            .await
            .with_field("ClientSettings.overrides.<count>")? as usize;
        let mut overrides = HashMap::with_capacity(count as usize);
        for _ in 0..count {
            let key = read_string(r)
                .await
                .with_field("ClientSettings.overrides[].key")?;
            let value = read_string(r)
                .await
                .with_field("ClientSettings.overrides[].value")?;
            overrides.insert(key, value);
        }
        overrides
    } else {
        HashMap::with_capacity(0)
    };

    Ok(ClientSettings {
        keep_failed,
        keep_going,
        try_fallback,
        verbosity,
        max_build_jobs,
        max_silent_time,
        verbose_build,
        build_cores,
        use_substitutes,
        overrides,
    })
}
/// Writes a ClientSettings structure to the stream.
#[instrument(skip(w, cs), level = "trace")]
pub async fn write_client_settings<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    proto: Proto,
    cs: &ClientSettings,
) -> Result<()> {
    write_bool(w, cs.keep_failed)
        .await
        .with_field("ClientSettings.keep_failed")?;
    write_bool(w, cs.keep_going)
        .await
        .with_field("ClientSettings.keep_going")?;
    write_bool(w, cs.try_fallback)
        .await
        .with_field("ClientSettings.try_fallback")?;

    write_verbosity(w, cs.verbosity)
        .await
        .with_field("ClientSettings.verbosity")?;
    write_u64(w, cs.max_build_jobs)
        .await
        .with_field("ClientSettings.max_build_jobs")?;
    write_u64(w, cs.max_silent_time)
        .await
        .with_field("ClientSettings.max_silent_time")?;
    write_u64(w, 0)
        .await
        .with_field("ClientSettings.__obsolete_use_build_hook")?;
    write_verbosity(
        w,
        match cs.verbose_build {
            true => Verbosity::Vomit,
            false => Verbosity::Error,
        },
    )
    .await
    .with_field("ClientSettings.verbose_build")?;
    write_u64(w, 0)
        .await
        .with_field("ClientSettings.__obsolete_log_type")?;
    write_u64(w, 0)
        .await
        .with_field("ClientSettings.__obsolete_print_build_trace")?;
    write_u64(w, cs.build_cores)
        .await
        .with_field("ClientSettings.build_cores")?;
    write_bool(w, cs.use_substitutes)
        .await
        .with_field("ClientSettings.use_substitutes")?;

    if proto >= Proto(1, 12) {
        write_u64(w, cs.overrides.len() as u64)
            .await
            .with_field("ClientSettings.overrides.<count>")?;
        for (key, value) in cs.overrides.iter() {
            write_string(w, key)
                .await
                .with_field("ClientSettings.overrides[].key")?;
            write_string(w, value)
                .await
                .with_field("ClientSettings.overrides[].value")?;
        }
    }

    Ok(())
}

/// Read a PathInfo structure from the stream.
#[instrument(skip(r), level = "trace")]
pub async fn read_pathinfo<R: AsyncReadExt + Unpin>(r: &mut R, proto: Proto) -> Result<PathInfo> {
    let deriver = read_string(r)
        .await
        .map(|s| (!s.is_empty()).then_some(s)) // "" -> None.
        .with_field("PathInfo.deriver")?;
    let nar_hash = read_string(r).await.with_field("PathInfo.nar_hash")?;
    let references = read_strings(r)
        .collect::<Result<Vec<_>>>()
        .await
        .with_field("PathInfo.deriver")?;
    let registration_time = read_datetime(r)
        .await
        .with_field("PathInfo.registration_time")?;
    let nar_size = read_u64(r).await.with_field("PathInfo.nar_size")?;

    let ultimate = OptionFuture::from(proto.since(16).then(|| read_bool(r)))
        .await
        .transpose()
        .with_field("PathInfo.ultimate")?
        .unwrap_or_default();
    let signatures = OptionFuture::from(proto.since(16).then(|| read_strings(r).collect()))
        .await
        .transpose()
        .with_field("PathInfo.signatures")?
        .unwrap_or_default();
    let ca = OptionFuture::from(proto.since(16).then(|| read_string(r)))
        .await
        .transpose()
        .with_field("PathInfo.ca")?
        .and_then(|s| (!s.is_empty()).then_some(s)); // "" -> None.

    Ok(PathInfo {
        deriver,
        nar_hash,
        references,
        registration_time,
        nar_size,
        ultimate,
        signatures,
        ca,
    })
}
/// Write a PathInfo structure to the stream.
#[instrument(skip(w, pi), level = "trace")]
pub async fn write_pathinfo<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    proto: Proto,
    pi: &PathInfo,
) -> Result<()> {
    write_string(w, pi.deriver.as_ref().map(|s| s.as_str()).unwrap_or(""))
        .await
        .with_field("PathInfo.deriver")?;
    write_string(w, pi.nar_hash.as_str())
        .await
        .with_field("PathInfo.nar_hash")?;
    write_strings(w, &pi.references)
        .await
        .with_field("PathInfo.deriver")?;
    write_u64(
        w,
        pi.registration_time.timestamp().try_into().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Timestamp too large: {}", e),
            )
        })?,
    )
    .await
    .with_field("PathInfo.registration_time")?;
    write_u64(w, pi.nar_size)
        .await
        .with_field("PathInfo.nar_size")?;

    if proto.since(16) {
        write_bool(w, pi.ultimate)
            .await
            .with_field("PathInfo.ultimate")?;
        write_strings(w, &pi.signatures)
            .await
            .with_field("PathInfo.signatures")?;
        write_string(w, &pi.ca.as_ref().map(|s| s.as_str()).unwrap_or(""))
            .await
            .with_field("PathInfo.ca")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::time::Duration;
    use tokio_stream::StreamExt;
    use tokio_test::io::Builder;

    fn pad_str<const L: usize>(s: &str) -> [u8; L] {
        assert!(L % 8 == 0, "{} is not aligned to 8", L);
        let mut v = [0u8; L];
        (&mut v[..s.len()]).copy_from_slice(s.as_bytes());
        v
    }

    #[tokio::test]
    async fn test_copy_to_framed_empty() {
        let mut r = Builder::new().read(&[]).build();
        let mut w = Builder::new().write(&0u64.to_le_bytes()).build();
        let mut buf = [0u8; 64];
        copy_to_framed(&mut r, &mut w, &mut buf).await.unwrap();
    }

    #[tokio::test]
    async fn test_copy_to_framed_1() {
        let mut r = Builder::new().read(&[1, 2, 3, 4]).build();
        let mut w = Builder::new()
            .write(&4u64.to_le_bytes())
            .write(&[1, 2, 3, 4])
            .write(&0u64.to_le_bytes())
            .build();
        let mut buf = [0u8; 64];
        copy_to_framed(&mut r, &mut w, &mut buf).await.unwrap();
    }

    #[tokio::test]
    async fn test_copy_to_framed_2reads() {
        // 10 bytes split across 2 reads.
        let mut r = Builder::new()
            .read(&[1, 2, 3, 4])
            .read(&[5, 6, 7, 8, 9, 10])
            .build();
        let mut w = Builder::new()
            .write(&4u64.to_le_bytes())
            .write(&[1, 2, 3, 4])
            .write(&6u64.to_le_bytes())
            .write(&[5, 6, 7, 8, 9, 10])
            .write(&0u64.to_le_bytes())
            .build();
        let mut buf = [0u8; 64];
        copy_to_framed(&mut r, &mut w, &mut buf).await.unwrap();
    }

    #[tokio::test]
    async fn test_copy_to_framed_3buffers() {
        // 5 bytes available, but buffer is only 2 bytes.
        let mut r = Builder::new().read(&[1, 2, 3, 4, 5]).build();
        let mut w = Builder::new()
            .write(&2u64.to_le_bytes())
            .write(&[1, 2])
            .write(&2u64.to_le_bytes())
            .write(&[3, 4])
            .write(&1u64.to_le_bytes())
            .write(&[5])
            .write(&0u64.to_le_bytes())
            .build();
        let mut buf = [0u8; 2];
        copy_to_framed(&mut r, &mut w, &mut buf).await.unwrap();
    }

    // Integers.
    #[tokio::test]
    async fn test_read_u64() {
        let mut mock = Builder::new().read(&1234567890u64.to_le_bytes()).build();
        assert_eq!(1234567890u64, read_u64(&mut mock).await.unwrap());
    }
    #[tokio::test]
    async fn test_write_u64() {
        let mut mock = Builder::new().write(&1234567890u64.to_le_bytes()).build();
        write_u64(&mut mock, 1234567890).await.unwrap();
    }

    // Booleans.
    #[tokio::test]
    async fn test_read_bool_0() {
        let mut mock = Builder::new().read(&0u64.to_le_bytes()).build();
        assert_eq!(false, read_bool(&mut mock).await.unwrap());
    }
    #[tokio::test]
    async fn test_read_bool_1() {
        let mut mock = Builder::new().read(&1u64.to_le_bytes()).build();
        assert_eq!(true, read_bool(&mut mock).await.unwrap());
    }
    #[tokio::test]
    async fn test_read_bool_2() {
        let mut mock = Builder::new().read(&2u64.to_le_bytes()).build();
        assert_eq!(true, read_bool(&mut mock).await.unwrap());
    }

    #[tokio::test]
    async fn test_write_bool_false() {
        let mut mock = Builder::new().write(&0u64.to_le_bytes()).build();
        write_bool(&mut mock, false).await.unwrap();
    }
    #[tokio::test]
    async fn test_write_bool_true() {
        let mut mock = Builder::new().write(&1u64.to_le_bytes()).build();
        write_bool(&mut mock, true).await.unwrap();
    }

    // Protocol versions.
    #[tokio::test]
    async fn test_read_proto() {
        // Why are they this way around?? Is this right?
        let mut mock = Builder::new().read(&[34, 12, 0, 0, 0, 0, 0, 0]).build();
        assert_eq!(Proto(12, 34), read_proto(&mut mock).await.unwrap());
    }
    #[tokio::test]
    async fn test_write_proto() {
        let mut mock = Builder::new().write(&[34, 12, 0, 0, 0, 0, 0, 0]).build();
        write_proto(&mut mock, Proto(12, 34)).await.unwrap();
    }

    // Verbosity.
    #[tokio::test]
    async fn test_read_verbosity() {
        let mut m = Builder::new()
            .read(&0u64.to_le_bytes()) // Error
            .read(&1u64.to_le_bytes()) // Warn
            .read(&2u64.to_le_bytes()) // Notice
            .read(&3u64.to_le_bytes()) // Info
            .read(&4u64.to_le_bytes()) // Talkative
            .read(&5u64.to_le_bytes()) // Chatty
            .read(&6u64.to_le_bytes()) // Debug
            .read(&7u64.to_le_bytes()) // Vomit
            .build();
        assert_eq!(Verbosity::Error, read_verbosity(&mut m).await.unwrap());
        assert_eq!(Verbosity::Warn, read_verbosity(&mut m).await.unwrap());
        assert_eq!(Verbosity::Notice, read_verbosity(&mut m).await.unwrap());
        assert_eq!(Verbosity::Info, read_verbosity(&mut m).await.unwrap());
        assert_eq!(Verbosity::Talkative, read_verbosity(&mut m).await.unwrap());
        assert_eq!(Verbosity::Chatty, read_verbosity(&mut m).await.unwrap());
        assert_eq!(Verbosity::Debug, read_verbosity(&mut m).await.unwrap());
        assert_eq!(Verbosity::Vomit, read_verbosity(&mut m).await.unwrap());
    }
    #[tokio::test]
    async fn test_write_verbosity() {
        let mut m = Builder::new()
            .write(&0u64.to_le_bytes()) // Error
            .write(&1u64.to_le_bytes()) // Warn
            .write(&2u64.to_le_bytes()) // Notice
            .write(&3u64.to_le_bytes()) // Info
            .write(&4u64.to_le_bytes()) // Talkative
            .write(&5u64.to_le_bytes()) // Chatty
            .write(&6u64.to_le_bytes()) // Debug
            .write(&7u64.to_le_bytes()) // Vomit
            .build();
        write_verbosity(&mut m, Verbosity::Error).await.unwrap();
        write_verbosity(&mut m, Verbosity::Warn).await.unwrap();
        write_verbosity(&mut m, Verbosity::Notice).await.unwrap();
        write_verbosity(&mut m, Verbosity::Info).await.unwrap();
        write_verbosity(&mut m, Verbosity::Talkative).await.unwrap();
        write_verbosity(&mut m, Verbosity::Chatty).await.unwrap();
        write_verbosity(&mut m, Verbosity::Debug).await.unwrap();
        write_verbosity(&mut m, Verbosity::Vomit).await.unwrap();
    }

    // Short strings.
    #[tokio::test]
    async fn test_read_string_len_0() {
        let mut mock = Builder::new().read(&0u64.to_le_bytes()).build();
        assert_eq!("".to_string(), read_string(&mut mock).await.unwrap());
    }
    #[tokio::test]
    async fn test_read_string_len_1() {
        let mut mock = Builder::new()
            .read(&1u64.to_le_bytes())
            .read("a".as_bytes())
            .read(&[0u8; 7])
            .build();
        assert_eq!("a".to_string(), read_string(&mut mock).await.unwrap());
    }
    #[tokio::test]
    async fn test_read_string_len_8() {
        let mut mock = Builder::new()
            .read(&8u64.to_le_bytes())
            .read("i'm gay.".as_bytes())
            .build();
        assert_eq!(
            "i'm gay.".to_string(),
            read_string(&mut mock).await.unwrap()
        );
    }

    #[tokio::test]
    async fn test_write_string_len_0() {
        let mut mock = Builder::new().write(&0u64.to_le_bytes()).build();
        write_string(&mut mock, "").await.unwrap();
    }
    #[tokio::test]
    async fn test_write_string_len_1() {
        let mut mock = Builder::new()
            .write(&1u64.to_le_bytes())
            .write("a\0\0\0\0\0\0\0".as_bytes())
            .build();
        write_string(&mut mock, "a").await.unwrap();
    }
    #[tokio::test]
    async fn test_write_string_len_8() {
        let mut mock = Builder::new()
            .write(&8u64.to_le_bytes())
            .write("i'm gay.".as_bytes())
            .build();
        write_string(&mut mock, "i'm gay.").await.unwrap();
    }

    // Long strings (infinite screaming).
    #[tokio::test]
    async fn test_read_string_len_1024() {
        let mut mock = Builder::new()
            .read(&1024u64.to_le_bytes())
            .read(&['a' as u8; 1024])
            .build();
        assert_eq!(
            String::from_iter(std::iter::repeat('a').take(1024)),
            read_string(&mut mock).await.unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_1025() {
        let mut mock = Builder::new()
            .read(&1025u64.to_le_bytes())
            .read(&['a' as u8; 1025])
            .read(&[0u8; 7])
            .build();
        assert_eq!(
            String::from_iter(std::iter::repeat('a').take(1025)),
            read_string(&mut mock).await.unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_2048() {
        let mut mock = Builder::new()
            .read(&2048u64.to_le_bytes())
            .read(&['a' as u8; 2048])
            .build();
        assert_eq!(
            String::from_iter(std::iter::repeat('a').take(2048)),
            read_string(&mut mock).await.unwrap()
        );
    }

    #[tokio::test]
    async fn test_read_strings_0() {
        let mut mock = Builder::new().read(&0u64.to_le_bytes()).build();
        assert_eq!(
            Vec::<String>::new(),
            read_strings(&mut mock)
                .collect::<Result<Vec<_>>>()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_strings_1() {
        let mut mock = Builder::new()
            .read(&1u64.to_le_bytes())
            .read(&8u64.to_le_bytes())
            .read("i'm gay.".as_bytes())
            .build();
        assert_eq!(
            vec!["i'm gay.".to_string()],
            read_strings(&mut mock)
                .collect::<Result<Vec<_>>>()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_strings_4() {
        let mut mock = Builder::new()
            .read(&4u64.to_le_bytes())
            .read(&22u64.to_le_bytes())
            .read("according to all known\0\0".as_bytes())
            .read(&16u64.to_le_bytes())
            .read("laws of aviation".as_bytes())
            .read(&25u64.to_le_bytes())
            .read("there's no way that a bee\0\0\0\0\0\0\0".as_bytes())
            .read(&21u64.to_le_bytes())
            .read("should be able to fly\0\0\0".as_bytes())
            .build();
        assert_eq!(
            vec![
                "according to all known".to_string(),
                "laws of aviation".to_string(),
                "there's no way that a bee".to_string(),
                "should be able to fly".to_string()
            ],
            read_strings(&mut mock)
                .collect::<Result<Vec<_>>>()
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_read_pathinfo_derived() {
        let mut mock = Builder::new()
            .read(&61u64.to_le_bytes()) // deriver
            .read(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-sqlite-3.43.2.drv",
            ))
            .read(&51u64.to_le_bytes()) // nar_hash
            .read(&pad_str::<56>(
                "sha256-sUu8vqpIoy7ZpnQPcwvQasNqX2jJOSXeEwd1yFtTukU=",
            ))
            .read(&2u64.to_le_bytes()) // references[]
            .read(&52u64.to_le_bytes()) // references[0]
            .read(&pad_str::<56>(
                "/nix/store/ffffffffffffffffffffffffffffffff-zlib-1.3",
            ))
             .read(&57u64.to_le_bytes()) // references[1]
             .read(&pad_str::<64>(
                 "/nix/store/ffffffffffffffffffffffffffffffff-glibc-2.38-27",
             ))
             .read(&1700495600u64.to_le_bytes()) // registration_time
             .read(&1768960u64.to_le_bytes()) // nar_size
             .read(&0u64.to_le_bytes()) // ultimate
             .read(&1u64.to_le_bytes()) // signatures[]
             .read(&106u64.to_le_bytes()) // signatures[0]
             .read(&pad_str::<112>(
                 "cache.nixos.org-1:Efz+S0y30Eny+nbjeiS0vlUiEpmNbW+m1CiznlC5odPRpTfQUENj+AQcDsnEgvXmaTY9OqG0l5pMIBc6XAk6AQ==",
             ))
             .read(&0u64.to_le_bytes()) // ca
            .build();
        assert_eq!(
            PathInfo {
                deriver: Some(
                    "/nix/store/ffffffffffffffffffffffffffffffff-sqlite-3.43.2.drv".into()
                ),
                nar_hash: "sha256-sUu8vqpIoy7ZpnQPcwvQasNqX2jJOSXeEwd1yFtTukU=".into(),
                references: vec![
                    "/nix/store/ffffffffffffffffffffffffffffffff-zlib-1.3".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-glibc-2.38-27".into(),
                ],
                registration_time: Utc.with_ymd_and_hms(2023, 11, 20, 15, 53, 20).unwrap(),
                nar_size: 1768960,
                ultimate: false,
                signatures: vec![
                    "cache.nixos.org-1:Efz+S0y30Eny+nbjeiS0vlUiEpmNbW+m1CiznlC5odPRpTfQUENj+AQcDsnEgvXmaTY9OqG0l5pMIBc6XAk6AQ==".into(),
                ],
                ca: None,
            },
            read_pathinfo(&mut mock, Proto(1, 35)).await.unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_pathinfo_ca() {
        let mut mock = Builder::new()
            .read(&0u64.to_le_bytes()) // deriver
            .read(&51u64.to_le_bytes()) // nar_hash
            .read(&pad_str::<56>(
                "sha256-1JmbR4NOsYNvgbJlqjp+4/bfm22IvhakiE1DXNfx78s=",
            ))
            .read(&5u64.to_le_bytes()) // references[]
            .read(&60u64.to_le_bytes()) // references[0]
            .read(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-bash-5.2-p15.drv",
            ))
            .read(&58u64.to_le_bytes()) // references[1]
            .read(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-curl-8.4.0.drv",
            ))
            .read(&54u64.to_le_bytes()) // references[2]
            .read(&pad_str::<56>(
                "/nix/store/ffffffffffffffffffffffffffffffff-builder.sh",
            ))
            .read(&60u64.to_le_bytes()) // references[3]
            .read(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-stdenv-linux.drv",
            ))
            .read(&60u64.to_le_bytes()) // references[4]
            .read(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-mirrors-list.drv",
            ))
            .read(&1700854586u64.to_le_bytes()) // registration_time
            .read(&3008u64.to_le_bytes()) // nar_size
            .read(&0u64.to_le_bytes()) // ultimate
            .read(&0u64.to_le_bytes()) // signatures[]
            .read(&64u64.to_le_bytes()) // ca
            .read(&pad_str::<64>(
                "text:sha256:0yjycizc8v9950dz9a69a7qlzcba9gl2gls8svi1g1i75xxf206d",
            ))
            .build();
        assert_eq!(
            PathInfo {
                deriver: None,
                nar_hash: "sha256-1JmbR4NOsYNvgbJlqjp+4/bfm22IvhakiE1DXNfx78s=".into(),
                references: vec![
                    "/nix/store/ffffffffffffffffffffffffffffffff-bash-5.2-p15.drv".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-curl-8.4.0.drv".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-builder.sh".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-stdenv-linux.drv".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-mirrors-list.drv".into(),
                ],
                registration_time: Utc.with_ymd_and_hms(2023, 11, 24, 19, 36, 26).unwrap(),
                nar_size: 3008,
                ultimate: false,
                signatures: vec![],
                ca: Some("text:sha256:0yjycizc8v9950dz9a69a7qlzcba9gl2gls8svi1g1i75xxf206d".into()),
            },
            read_pathinfo(&mut mock, Proto(1, 35)).await.unwrap()
        );
    }

    #[tokio::test]
    async fn test_write_pathinfo_derived() {
        let mut mock = Builder::new()
            .write(&61u64.to_le_bytes()) // deriver
            .write(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-sqlite-3.43.2.drv",
            ))
            .write(&51u64.to_le_bytes()) // nar_hash
            .write(&pad_str::<56>(
                "sha256-sUu8vqpIoy7ZpnQPcwvQasNqX2jJOSXeEwd1yFtTukU=",
            ))
            .write(&2u64.to_le_bytes()) // references[]
            .write(&52u64.to_le_bytes()) // references[0]
            .write(&pad_str::<56>(
                "/nix/store/ffffffffffffffffffffffffffffffff-zlib-1.3",
            ))
            .write(&57u64.to_le_bytes()) // references[1]
            .write(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-glibc-2.38-27",
            ))
            .write(&1700495600u64.to_le_bytes()) // registration_time
            .write(&1768960u64.to_le_bytes()) // nar_size
            .write(&0u64.to_le_bytes()) // ultimate
            .write(&1u64.to_le_bytes()) // signatures[]
            .write(&106u64.to_le_bytes()) // signatures[0]
            .write(&pad_str::<112>(
                 "cache.nixos.org-1:Efz+S0y30Eny+nbjeiS0vlUiEpmNbW+m1CiznlC5odPRpTfQUENj+AQcDsnEgvXmaTY9OqG0l5pMIBc6XAk6AQ==",
             ))
            .write(&0u64.to_le_bytes()) // ca
            .build();
        write_pathinfo(
            &mut mock,
            Proto(1, 35),
            &PathInfo {
                deriver: Some(
                    "/nix/store/ffffffffffffffffffffffffffffffff-sqlite-3.43.2.drv".into(),
                ),
                nar_hash: "sha256-sUu8vqpIoy7ZpnQPcwvQasNqX2jJOSXeEwd1yFtTukU=".into(),
                references: vec![
                    "/nix/store/ffffffffffffffffffffffffffffffff-zlib-1.3".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-glibc-2.38-27".into(),
                ],
                registration_time: Utc.with_ymd_and_hms(2023, 11, 20, 15, 53, 20).unwrap(),
                nar_size: 1768960,
                ultimate: false,
                signatures: vec![
                   "cache.nixos.org-1:Efz+S0y30Eny+nbjeiS0vlUiEpmNbW+m1CiznlC5odPRpTfQUENj+AQcDsnEgvXmaTY9OqG0l5pMIBc6XAk6AQ==".into(),
                ],
                ca: None,
            },
        )
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn test_write_pathinfo_ca() {
        let mut mock = Builder::new()
            .write(&0u64.to_le_bytes()) // deriver
            .write(&51u64.to_le_bytes()) // nar_hash
            .write(&pad_str::<56>(
                "sha256-1JmbR4NOsYNvgbJlqjp+4/bfm22IvhakiE1DXNfx78s=",
            ))
            .write(&5u64.to_le_bytes()) // references[]
            .write(&60u64.to_le_bytes()) // references[0]
            .write(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-bash-5.2-p15.drv",
            ))
            .write(&58u64.to_le_bytes()) // references[1]
            .write(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-curl-8.4.0.drv",
            ))
            .write(&54u64.to_le_bytes()) // references[2]
            .write(&pad_str::<56>(
                "/nix/store/ffffffffffffffffffffffffffffffff-builder.sh",
            ))
            .write(&60u64.to_le_bytes()) // references[3]
            .write(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-stdenv-linux.drv",
            ))
            .write(&60u64.to_le_bytes()) // references[4]
            .write(&pad_str::<64>(
                "/nix/store/ffffffffffffffffffffffffffffffff-mirrors-list.drv",
            ))
            .write(&1700854586u64.to_le_bytes()) // registration_time
            .write(&3008u64.to_le_bytes()) // nar_size
            .write(&0u64.to_le_bytes()) // ultimate
            .write(&0u64.to_le_bytes()) // signatures[]
            .write(&64u64.to_le_bytes()) // ca
            .write(&pad_str::<64>(
                "text:sha256:0yjycizc8v9950dz9a69a7qlzcba9gl2gls8svi1g1i75xxf206d",
            ))
            .build();
        write_pathinfo(
            &mut mock,
            Proto(1, 35),
            &PathInfo {
                deriver: None,
                nar_hash: "sha256-1JmbR4NOsYNvgbJlqjp+4/bfm22IvhakiE1DXNfx78s=".into(),
                references: vec![
                    "/nix/store/ffffffffffffffffffffffffffffffff-bash-5.2-p15.drv".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-curl-8.4.0.drv".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-builder.sh".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-stdenv-linux.drv".into(),
                    "/nix/store/ffffffffffffffffffffffffffffffff-mirrors-list.drv".into(),
                ],
                registration_time: Utc.with_ymd_and_hms(2023, 11, 24, 19, 36, 26).unwrap(),
                nar_size: 3008,
                ultimate: false,
                signatures: vec![],
                ca: Some("text:sha256:0yjycizc8v9950dz9a69a7qlzcba9gl2gls8svi1g1i75xxf206d".into()),
            },
        )
        .await
        .unwrap();
    }

    // This test case was adapted from cppnix (LGPL license) commit
    // 91b6833686a6a6d9eac7f3f66393ec89ef1d3b57
    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_cppnix__src_libstore_worker_protocol__string() {
        let mut mock = Builder::new()
            .write(&[
                // cppnix ./tests/unit/libstore/data/common-protocol/string.bin
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x68, 0x69, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x77, 0x68, 0x69, 0x74, 0x65, 0x20, 0x72, 0x61, 0x62, 0x62,
                0x69, 0x74, 0x00, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0xe5, 0xa4, 0xa7, 0xe7, 0x99, 0xbd, 0xe5, 0x85, 0x94, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6f, 0x68, 0x20, 0x6e,
                0x6f, 0x20, 0x00, 0x00,
            ])
            .build();

        // cppnix tests/unit/libstore/worker-protocol.cc
        write_string(&mut mock, "").await.unwrap();
        write_string(&mut mock, "hi").await.unwrap();
        write_string(&mut mock, "white rabbit").await.unwrap();
        write_string(&mut mock, "大白兔").await.unwrap();
        write_string(&mut mock, "oh no \0\0\0 what was that!")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_framedreader_empty() {
        let mut mock = Builder::new().read(&0u64.to_le_bytes()).build();
        let mut buf = Vec::new();
        let len = FramedReader::new(&mut mock)
            .read_to_end(&mut buf)
            .await
            .unwrap();
        assert_eq!(0, len);
        assert_eq!(0, buf.len());
    }

    #[tokio::test]
    async fn test_framedreader_1f() {
        let mut mock = Builder::new()
            .read(&2u64.to_le_bytes())
            .read(&[1, 2])
            .read(&0u64.to_le_bytes())
            .build();
        let mut buf = Vec::new();
        let len = FramedReader::new(&mut mock)
            .read_to_end(&mut buf)
            .await
            .unwrap();
        assert_eq!(&[1, 2], &buf[..]);
        assert_eq!(2, len);
    }

    #[tokio::test]
    async fn test_framedreader_2f() {
        let mut mock = Builder::new()
            .read(&2u64.to_le_bytes())
            .read(&[1, 2])
            .read(&4u64.to_le_bytes())
            .read(&[3, 4, 5, 6])
            .read(&0u64.to_le_bytes())
            .build();
        let mut buf = Vec::new();
        let len = FramedReader::new(&mut mock)
            .read_to_end(&mut buf)
            .await
            .unwrap();
        assert_eq!(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06], &buf[..]);
        assert_eq!(6, len);
    }

    #[tokio::test]
    async fn test_framedreader_2f_wait() {
        let mut mock = Builder::new()
            .read(&2u64.to_le_bytes())
            .read(&[1, 2])
            .wait(Duration::from_millis(100))
            .read(&4u64.to_le_bytes())
            .read(&[3, 4, 5, 6])
            .read(&0u64.to_le_bytes())
            .build();
        let mut buf = Vec::new();
        let len = FramedReader::new(&mut mock)
            .read_to_end(&mut buf)
            .await
            .unwrap();
        assert_eq!(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06], &buf[..]);
        assert_eq!(6, len);
    }

    #[tokio::test]
    async fn test_framedreader_2f_overflow() {
        let mut mock = Builder::new()
            .read(&2u64.to_le_bytes())
            .read(&[1, 2])
            .read(&4u64.to_le_bytes())
            .read(&[3, 4, 5, 6])
            .read(&0u64.to_le_bytes())
            .build();
        let mut buf = [0u8; 2];
        let mut r = FramedReader::new(&mut mock);
        assert_eq!(2, r.read(&mut buf).await.unwrap());
        assert_eq!(&[1, 2], &buf[..]);
        assert_eq!(2, r.read(&mut buf).await.unwrap());
        assert_eq!(&[3, 4], &buf[..]);
        assert_eq!(2, r.read(&mut buf).await.unwrap());
        assert_eq!(&[5, 6], &buf[..]);
        assert_eq!(0, r.read(&mut buf).await.unwrap());
    }
}
