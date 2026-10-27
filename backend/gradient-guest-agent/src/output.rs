/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

const READ_CHUNK: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Default)]
struct Written {
    bytes: u64,
    closed: bool,
}

pub struct OutputLog {
    file: File,
    written: u64,
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
    pub async fn create(path: &Path) -> Result<Self> {
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
            appended,
            readers: OutputReaders {
                path: path.to_owned(),
                appended: receiver,
            },
        })
    }

    pub async fn append(&mut self, data: &[u8]) -> Result<()> {
        self.file.write_all(data).await?;
        self.file.flush().await?;
        self.written += data.len() as u64;
        let bytes = self.written;
        self.appended.send_modify(|written| written.bytes = bytes);
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
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        loop {
            let written = *self.appended.borrow_and_update();
            if self.offset < written.bytes {
                return self.read_up_to(written.bytes);
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
        self.offset += n as u64;
        Some(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::OutputLog;

    #[tokio::test]
    async fn a_reader_from_the_middle_of_a_write_gets_only_its_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = OutputLog::create(&dir.path().join("output.log"))
            .await
            .expect("create");
        log.append(b"hello world").await.expect("append");

        let mut reader = log.readers().reader_from(6);
        assert_eq!(reader.next().await, Some(b"world".to_vec()));
    }

    #[tokio::test]
    async fn a_reader_gets_writes_that_come_after_it_started() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = OutputLog::create(&dir.path().join("output.log"))
            .await
            .expect("create");
        let mut reader = log.readers().reader_from(0);

        let collect = async {
            let mut bytes = Vec::new();
            while let Some(chunk) = reader.next().await {
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
