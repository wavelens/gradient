/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::SeekFrom;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nix::fcntl::{FallocateFlags, fallocate};
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::watch;

const READ_CHUNK: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Default)]
struct Written {
    bytes: u64,
    kept_from: u64,
    closed: bool,
}

#[derive(Debug, PartialEq)]
pub enum OutputChunk {
    Data(Vec<u8>),
    Dropped(u64),
}

pub struct OutputLog {
    file: File,
    written: u64,
    kept_from: u64,
    kept: u64,
    appended: watch::Sender<Written>,
    readers: OutputReaders,
}

#[derive(Clone)]
pub struct OutputReaders {
    path: PathBuf,
    appended: watch::Receiver<Written>,
}

pub struct OutputReader {
    path: PathBuf,
    offset: u64,
    file: Option<std::fs::File>,
    appended: watch::Receiver<Written>,
}

impl OutputLog {
    pub async fn create(path: &Path, kept: u64) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let file = File::create(path)
            .await
            .with_context(|| format!("create {}", path.display()))?;
        let (appended, receiver) = watch::channel(Written::default());
        Ok(Self {
            file,
            written: 0,
            kept_from: 0,
            kept,
            appended,
            readers: OutputReaders {
                path: path.to_owned(),
                appended: receiver,
            },
        })
    }

    pub async fn append(&mut self, data: &[u8]) -> Result<()> {
        if let Err(e) = self.write(data).await {
            self.file.set_len(self.written).await?;
            self.file.seek(SeekFrom::Start(self.written)).await?;
            return Err(e);
        }
        self.written += data.len() as u64;
        if let Err(e) = self.drop_beyond_kept() {
            tracing::warn!(error = %format!("{e:#}"), "cannot drop old program output");
        }
        let (bytes, kept_from) = (self.written, self.kept_from);
        self.appended.send_modify(|written| {
            written.bytes = bytes;
            written.kept_from = kept_from;
        });
        Ok(())
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.file.write_all(data).await?;
        self.file.flush().await?;
        Ok(())
    }

    // Punching a hole frees the tmpfs pages while every offset stays valid.
    fn drop_beyond_kept(&mut self) -> Result<()> {
        if self.written - self.kept_from <= self.kept {
            return Ok(());
        }

        let kept_from = self.written - self.kept / 2;
        fallocate(
            &self.file,
            FallocateFlags::FALLOC_FL_PUNCH_HOLE | FallocateFlags::FALLOC_FL_KEEP_SIZE,
            i64::try_from(self.kept_from)?,
            i64::try_from(kept_from - self.kept_from)?,
        )
        .context("punch a hole into the output file")?;
        self.kept_from = kept_from;
        Ok(())
    }

    pub fn readers(&self) -> OutputReaders {
        self.readers.clone()
    }

    pub fn close(&self) {
        self.appended.send_modify(|written| written.closed = true);
    }
}

impl OutputReaders {
    pub fn reader_from(&self, offset: u64) -> OutputReader {
        OutputReader {
            path: self.path.clone(),
            offset,
            file: None,
            appended: self.appended.clone(),
        }
    }
}

impl OutputReader {
    pub async fn next(&mut self) -> Option<OutputChunk> {
        loop {
            let written = *self.appended.borrow_and_update();
            if self.offset < written.kept_from {
                let dropped = written.kept_from - self.offset;
                self.offset = written.kept_from;
                return Some(OutputChunk::Dropped(dropped));
            }

            if self.offset < written.bytes {
                let data = self.read_up_to(written.bytes)?;
                if self.appended.borrow().kept_from > self.offset {
                    continue;
                }
                self.offset += data.len() as u64;
                return Some(OutputChunk::Data(data));
            }

            if written.closed || self.appended.changed().await.is_err() {
                return None;
            }
        }
    }

    // A positional read without an await keeps `next` cancel safe inside a `select!`.
    fn read_up_to(&mut self, end: u64) -> Option<Vec<u8>> {
        let file = match self.file.as_mut() {
            Some(file) => file,
            None => self.file.insert(std::fs::File::open(&self.path).ok()?),
        };

        let len = usize::try_from((end - self.offset).min(READ_CHUNK)).ok()?;
        let mut buf = vec![0; len];
        let n = file.read_at(&mut buf, self.offset).ok()?;
        if n == 0 {
            return None;
        }
        buf.truncate(n);
        Some(buf)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::{OutputChunk, OutputLog};

    async fn output_log(dir: &tempfile::TempDir, kept: u64) -> OutputLog {
        OutputLog::create(&dir.path().join("output.log"), kept)
            .await
            .expect("create")
    }

    #[tokio::test]
    async fn a_reader_from_the_middle_of_a_write_gets_only_its_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = output_log(&dir, 1024).await;
        log.append(b"hello world").await.expect("append");

        let mut reader = log.readers().reader_from(6);
        assert_eq!(
            reader.next().await,
            Some(OutputChunk::Data(b"world".to_vec()))
        );
    }

    #[tokio::test]
    async fn a_reader_behind_the_kept_output_learns_how_much_was_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = output_log(&dir, 8).await;
        log.append(b"0123456789").await.expect("append");

        let mut reader = log.readers().reader_from(2);
        assert_eq!(reader.next().await, Some(OutputChunk::Dropped(4)));
        assert_eq!(
            reader.next().await,
            Some(OutputChunk::Data(b"6789".to_vec()))
        );
    }

    #[tokio::test]
    async fn dropped_output_no_longer_takes_space() {
        let dir = tempfile::tempdir().expect("tempdir");
        let kept = 256 * 1024;
        let mut log = output_log(&dir, kept).await;
        for _ in 0..64 {
            log.append(&[b'x'; 64 * 1024]).await.expect("append");
        }

        let metadata = std::fs::metadata(dir.path().join("output.log")).expect("metadata");
        assert_eq!(metadata.len(), 64 * 64 * 1024);
        assert!(metadata.blocks() * 512 <= 2 * kept);
    }

    #[tokio::test]
    async fn a_reader_gets_writes_that_come_after_it_started() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = output_log(&dir, 1024).await;
        let mut reader = log.readers().reader_from(0);

        let collect = async {
            let mut bytes = Vec::new();
            while let Some(OutputChunk::Data(chunk)) = reader.next().await {
                bytes.extend(chunk);
            }
            bytes
        };
        let write = async {
            log.append(b"first ").await.expect("append");
            log.append(b"second").await.expect("append");
            log.close();
        };

        let (bytes, ()) = tokio::join!(collect, write);
        assert_eq!(bytes, b"first second".to_vec());
    }
}
