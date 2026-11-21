/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{CliError, FileAction};
use crate::logfmt::is_bidirectional_control_character;
use connector::ConnectorError;
use futures::{Stream, StreamExt, pin_mut};
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use tempfile::NamedTempFile;

pub fn checked_file_path_in_directory_from_server_file_name(
    directory: &Path,
    server_file_name: &str,
) -> Result<PathBuf, CliError> {
    let rejected_because = |rule| {
        Err(CliError::ServerDownloadFileNameRejectedBySafetyRule {
            name: server_file_name.to_owned(),
            rule,
        })
    };
    if server_file_name.is_empty() {
        return rejected_because("is empty");
    }
    if server_file_name.contains(['/', '\\']) {
        return rejected_because("contains `/` or `\\`");
    }
    if server_file_name.chars().any(char::is_control) {
        return rejected_because("does contain a control character");
    }
    if server_file_name
        .chars()
        .any(is_bidirectional_control_character)
    {
        return rejected_because("with characters that change the direction of text");
    }
    if server_file_name.starts_with('.') {
        return rejected_because("starts with `.`");
    }
    if server_file_name.contains(':') {
        return rejected_because("with `:`");
    }
    if server_file_name.ends_with(['.', ' ']) {
        return rejected_because("ends with `.` or a space");
    }
    if is_windows_reserved_device_name(server_file_name) {
        return rejected_because("is a reserved device name on Windows");
    }
    let mut components = Path::new(server_file_name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(directory.join(server_file_name)),
        _ => rejected_because("is not a plain file name"),
    }
}

fn is_windows_reserved_device_name(file_name: &str) -> bool {
    let stem = file_name
        .split_once('.')
        .map_or(file_name, |(stem, _)| stem)
        .to_ascii_uppercase();
    match stem.as_bytes() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" => true,
        [b'C', b'O', b'M', digit] | [b'L', b'P', b'T', digit] => (b'1'..=b'9').contains(digit),
        _ => false,
    }
}

pub fn create_download_directory_or_verify_existing_directory(
    directory: &Path,
) -> Result<(), CliError> {
    let create_error = |e| CliError::file(FileAction::Create, directory, e);
    match std::fs::create_dir(directory) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(directory)
                .map_err(create_error)?
                .is_dir()
            {
                Ok(())
            } else {
                Err(CliError::DownloadsPathIsSymlinkOrNotDirectory(
                    directory.to_owned(),
                ))
            }
        }
        Err(e) => Err(create_error(e)),
    }
}

pub enum DownloadDestination {
    CommandLinePath(PathBuf),
    FileInServerNamedDirectory {
        path: PathBuf,
        replace_existing: bool,
    },
}

impl DownloadDestination {
    pub fn path(&self) -> &Path {
        match self {
            Self::CommandLinePath(path) | Self::FileInServerNamedDirectory { path, .. } => path,
        }
    }
}

pub async fn write_downloaded_byte_chunks_to_destination(
    chunks: impl Stream<Item = Result<impl AsRef<[u8]>, ConnectorError>>,
    destination: &DownloadDestination,
) -> Result<u64, CliError> {
    let path = destination.path();
    let write_error = |e| CliError::file(FileAction::Write, path, e);
    let mut opened = OpenedDownloadDestination::open(destination).map_err(write_error)?;
    pin_mut!(chunks);
    let mut written = 0u64;
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        opened
            .file()
            .write_all(chunk.as_ref())
            .map_err(write_error)?;
        written += chunk.as_ref().len() as u64;
    }
    opened.finish()?;
    Ok(written)
}

pub fn write_bytes_to_download_destination(
    bytes: &[u8],
    destination: &DownloadDestination,
) -> Result<(), CliError> {
    let path = destination.path();
    let write_error = |e| CliError::file(FileAction::Write, path, e);
    let mut opened = OpenedDownloadDestination::open(destination).map_err(write_error)?;
    opened.file().write_all(bytes).map_err(write_error)?;
    opened.finish()
}

enum OpenedDownloadDestination<'a> {
    PathTypedByUser(std::fs::File),
    TemporaryFileToRenameOntoTarget {
        staged: NamedTempFile,
        path: &'a Path,
        replace_existing: bool,
    },
}

impl<'a> OpenedDownloadDestination<'a> {
    fn open(destination: &'a DownloadDestination) -> io::Result<Self> {
        Ok(match destination {
            DownloadDestination::CommandLinePath(path) => {
                Self::PathTypedByUser(std::fs::File::create(path)?)
            }
            DownloadDestination::FileInServerNamedDirectory {
                path,
                replace_existing,
            } => Self::TemporaryFileToRenameOntoTarget {
                staged: create_temporary_file_for_atomic_rename_to_download_path(path)?,
                path,
                replace_existing: *replace_existing,
            },
        })
    }

    fn file(&mut self) -> &mut std::fs::File {
        match self {
            Self::PathTypedByUser(file) => file,
            Self::TemporaryFileToRenameOntoTarget { staged, .. } => staged.as_file_mut(),
        }
    }

    fn finish(self) -> Result<(), CliError> {
        match self {
            Self::PathTypedByUser(_) => Ok(()),
            Self::TemporaryFileToRenameOntoTarget {
                staged,
                path,
                replace_existing,
            } => rename_temp_download_to_final_path(staged, path, replace_existing),
        }
    }
}

fn create_temporary_file_for_atomic_rename_to_download_path(
    path: &Path,
) -> io::Result<NamedTempFile> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    builder.tempfile_in(directory)
}

// A rename can replace the symlink at `path` itself rather than follow the link and write into the target. No-clobber renames reject any existing entry.
fn rename_temp_download_to_final_path(
    staged: NamedTempFile,
    path: &Path,
    replace_existing: bool,
) -> Result<(), CliError> {
    let persisted = if replace_existing {
        staged.persist(path)
    } else {
        staged.persist_noclobber(path)
    };
    persisted.map(drop).map_err(|e| match e.error.kind() {
        io::ErrorKind::AlreadyExists => CliError::DownloadTargetFileAlreadyExists(path.to_owned()),
        _ => CliError::file(FileAction::Write, path, e.error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_server_named_file_download_destination(
        path: &Path,
        replace_existing: bool,
    ) -> DownloadDestination {
        DownloadDestination::FileInServerNamedDirectory {
            path: path.to_owned(),
            replace_existing,
        }
    }

    fn write_bytes_to_server_named_download_path_with_replace_flag(
        bytes: &[u8],
        path: &Path,
        replace_existing: bool,
    ) -> Result<(), CliError> {
        write_bytes_to_download_destination(
            bytes,
            &make_server_named_file_download_destination(path, replace_existing),
        )
    }

    fn write_bytes_as_one_chunk_stream_to_download_path_blocking(
        path: &Path,
        bytes: &[u8],
        replace_existing: bool,
    ) -> Result<u64, CliError> {
        let chunks = futures::stream::iter([Ok::<_, ConnectorError>(bytes.to_vec())]);
        futures::executor::block_on(write_downloaded_byte_chunks_to_destination(
            chunks,
            &make_server_named_file_download_destination(path, replace_existing),
        ))
    }

    #[cfg(unix)]
    #[test]
    fn writing_to_user_typed_path_follows_symlink_and_writing_to_dev_stdout_works() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_bytes_to_download_destination(
            b"new",
            &DownloadDestination::CommandLinePath(link.clone()),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());

        let chunks = futures::stream::iter([Ok::<_, ConnectorError>(b"\n".to_vec())]);
        let stdout = DownloadDestination::CommandLinePath(PathBuf::from("/dev/stdout"));
        assert_eq!(
            futures::executor::block_on(write_downloaded_byte_chunks_to_destination(
                chunks, &stdout
            ))
            .unwrap(),
            1
        );
    }

    #[test]
    fn server_file_names_that_leave_the_directory_or_hide_are_rejected() {
        let dir = Path::new("out");
        for name in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "a\\b",
            "/etc/passwd",
            ".envrc",
            ".bashrc",
            "a\0b",
            "a\nb",
            "a\u{1b}b",
            "a\u{9b}b",
        ] {
            assert!(
                matches!(
                    checked_file_path_in_directory_from_server_file_name(dir, name),
                    Err(CliError::ServerDownloadFileNameRejectedBySafetyRule { .. })
                ),
                "accepted {name:?}"
            );
        }
    }

    #[test]
    fn plain_server_file_name_is_joined_to_the_directory() {
        assert_eq!(
            checked_file_path_in_directory_from_server_file_name(Path::new("out"), "app.tar.gz")
                .unwrap(),
            PathBuf::from("out/app.tar.gz")
        );
    }

    #[test]
    fn reject_unsafe_download_file_names_and_accept_safe_ones() {
        for name in [
            "invoice\u{202e}fdp.exe",
            "a\u{2066}b",
            "a\u{200f}b",
            "CON",
            "con.txt",
            "Aux.tar.gz",
            "nul",
            "PRN",
            "com1",
            "COM9.log",
            "lpt1",
            "LPT9.txt",
            "file:stream",
            "trailing.",
            "trailing ",
        ] {
            assert!(
                matches!(
                    checked_file_path_in_directory_from_server_file_name(Path::new("out"), name),
                    Err(CliError::ServerDownloadFileNameRejectedBySafetyRule { .. })
                ),
                "accepted {name:?}"
            );
        }
        for name in ["console.txt", "con2", "nulled", "com10", "lpt", "a.b c"] {
            assert!(
                checked_file_path_in_directory_from_server_file_name(Path::new("out"), name)
                    .is_ok(),
                "rejected {name:?}"
            );
        }
    }

    #[test]
    fn existing_file_is_kept_without_replace_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        std::fs::write(&path, "old").unwrap();

        assert!(matches!(
            write_bytes_as_one_chunk_stream_to_download_path_blocking(&path, b"new", false),
            Err(CliError::DownloadTargetFileAlreadyExists(_))
        ));
        assert!(matches!(
            write_bytes_to_server_named_download_path_with_replace_flag(b"new", &path, false),
            Err(CliError::DownloadTargetFileAlreadyExists(_))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn replace_existing_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        std::fs::write(&path, "old").unwrap();

        assert_eq!(
            write_bytes_as_one_chunk_stream_to_download_path_blocking(&path, b"new", true).unwrap(),
            3
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn downloads_directory_is_created_when_missing_and_accepted_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path().join("downloads");

        create_download_directory_or_verify_existing_directory(&downloads).unwrap();
        create_download_directory_or_verify_existing_directory(&downloads).unwrap();
        assert!(downloads.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_downloads_path_that_is_symlink_or_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        let file = dir.path().join("file");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        std::fs::write(&file, "").unwrap();

        for path in [link, file] {
            assert!(matches!(
                create_download_directory_or_verify_existing_directory(&path),
                Err(CliError::DownloadsPathIsSymlinkOrNotDirectory(_))
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_at_the_target_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::fs::write(&target, "kept").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(
            write_bytes_to_server_named_download_path_with_replace_flag(b"new", &link, false)
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");

        write_bytes_to_server_named_download_path_with_replace_flag(b"new", &link, true).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");
        assert!(!std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "new");
    }
}
