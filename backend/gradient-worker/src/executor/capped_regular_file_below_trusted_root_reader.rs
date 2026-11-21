/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fs::File;
use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SizeCappedNoFollowReadError {
    #[error("{path} does not exist")]
    FileOrAncestorDirectoryNotFound { path: PathBuf },
    #[error("{path}: symlink or non-directory component in path")]
    PathComponentIsSymlinkOrIntermediateComponentIsNotDirectory { path: PathBuf },
    #[error("{path} is not a regular file")]
    FinalFileIsNotRegularFile { path: PathBuf },
    #[error("{path} is larger than {max_bytes} bytes")]
    FileIsLargerThanByteCap { path: PathBuf, max_bytes: u64 },
    #[error("Use a relative path for `{path}`. Use simple names only.")]
    RelativePathHasNonPlainNameComponent { path: PathBuf },
    #[error("Failed to open or read `{path}`")]
    OperatingSystemErrorWhileOpeningOrReading {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub(crate) async fn open_regular_file_below_root_and_read_at_most_max_bytes(
    root: &Path,
    relative: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>, SizeCappedNoFollowReadError> {
    let full = root.join(relative);
    let (root, relative) = (root.to_owned(), relative.to_owned());
    tokio::task::spawn_blocking(move || {
        read_capped_regular_file_without_following_links(&root, &relative, max_bytes)
    })
    .await
    .unwrap_or_else(|join| {
        Err(
            SizeCappedNoFollowReadError::OperatingSystemErrorWhileOpeningOrReading {
                path: full,
                source: std::io::Error::other(join),
            },
        )
    })
}

fn read_capped_regular_file_without_following_links(
    root: &Path,
    relative: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>, SizeCappedNoFollowReadError> {
    let full = root.join(relative);
    let names: Vec<&std::ffi::OsStr> = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Option<_>>()
        .filter(|names: &Vec<_>| !names.is_empty())
        .ok_or_else(
            || SizeCappedNoFollowReadError::RelativePathHasNonPlainNameComponent {
                path: relative.to_owned(),
            },
        )?;
    let refused = |errno: Errno| {
        match errno {
        Errno::ENOENT => SizeCappedNoFollowReadError::FileOrAncestorDirectoryNotFound {
            path: full.clone(),
        },
        Errno::ELOOP | Errno::ENOTDIR => {
            SizeCappedNoFollowReadError::PathComponentIsSymlinkOrIntermediateComponentIsNotDirectory {
                path: full.clone(),
            }
        }
        other => SizeCappedNoFollowReadError::OperatingSystemErrorWhileOpeningOrReading {
            path: full.clone(),
            source: other.into(),
        },
    }
    };

    let directory_flags =
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let (last, parents) = names.split_last().expect("at least one name");
    let mut directory: OwnedFd = open(root, directory_flags, Mode::empty()).map_err(refused)?;
    for name in parents {
        directory = openat(&directory, *name, directory_flags, Mode::empty()).map_err(refused)?;
    }
    let file: File = openat(
        &directory,
        *last,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(refused)?
    .into();

    let io_error =
        |source| SizeCappedNoFollowReadError::OperatingSystemErrorWhileOpeningOrReading {
            path: full.clone(),
            source,
        };
    if !file.metadata().map_err(io_error)?.file_type().is_file() {
        return Err(SizeCappedNoFollowReadError::FinalFileIsNotRegularFile { path: full });
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > max_bytes {
        return Err(SizeCappedNoFollowReadError::FileIsLargerThanByteCap {
            path: full,
            max_bytes,
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn call_capped_read_with_str_relative_path(
        root: &Path,
        relative: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, SizeCappedNoFollowReadError> {
        open_regular_file_below_root_and_read_at_most_max_bytes(
            root,
            Path::new(relative),
            max_bytes,
        )
        .await
    }

    #[tokio::test]
    async fn read_regular_file_in_subdirectory_below_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("dir")).unwrap();
        std::fs::write(root.path().join("dir/file"), b"content").unwrap();

        assert_eq!(
            call_capped_read_with_str_relative_path(root.path(), "dir/file", 7)
                .await
                .unwrap(),
            b"content"
        );
    }

    #[tokio::test]
    async fn symlink_to_dev_zero_as_final_path_component_is_refused() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/dev/zero", root.path().join("file")).unwrap();

        let refused = call_capped_read_with_str_relative_path(root.path(), "file", 1024).await;

        assert!(
            matches!(
                refused,
                Err(SizeCappedNoFollowReadError::PathComponentIsSymlinkOrIntermediateComponentIsNotDirectory { .. })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn symlinked_intermediate_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("file"), b"secret").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root.path().join("dir")).unwrap();

        let refused = call_capped_read_with_str_relative_path(root.path(), "dir/file", 1024).await;

        assert!(
            matches!(
                refused,
                Err(SizeCappedNoFollowReadError::PathComponentIsSymlinkOrIntermediateComponentIsNotDirectory { .. })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn symlinked_root_directory_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("file"), b"secret").unwrap();
        let root = parent.path().join("root");
        std::os::unix::fs::symlink(elsewhere.path(), &root).unwrap();

        let refused = call_capped_read_with_str_relative_path(&root, "file", 1024).await;

        assert!(
            matches!(
                refused,
                Err(SizeCappedNoFollowReadError::PathComponentIsSymlinkOrIntermediateComponentIsNotDirectory { .. })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn directory_as_final_path_component_is_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("dir")).unwrap();

        let refused = call_capped_read_with_str_relative_path(root.path(), "dir", 1024).await;

        assert!(
            matches!(
                refused,
                Err(SizeCappedNoFollowReadError::FinalFileIsNotRegularFile { .. })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn refuses_fifo_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        nix::unistd::mkfifo(&root.path().join("fifo"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();

        let refused = call_capped_read_with_str_relative_path(root.path(), "fifo", 1024).await;

        assert!(
            matches!(
                refused,
                Err(SizeCappedNoFollowReadError::FinalFileIsNotRegularFile { .. })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn regular_file_above_byte_cap_is_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), b"12345").unwrap();

        let refused = call_capped_read_with_str_relative_path(root.path(), "file", 4).await;

        assert!(
            matches!(
                refused,
                Err(SizeCappedNoFollowReadError::FileIsLargerThanByteCap { max_bytes: 4, .. })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn relative_path_with_parent_dir_leaving_trusted_root_is_refused() {
        let root = tempfile::tempdir().unwrap();

        let refused = call_capped_read_with_str_relative_path(root.path(), "../file", 4).await;

        assert!(
            matches!(
                refused,
                Err(SizeCappedNoFollowReadError::RelativePathHasNonPlainNameComponent { .. })
            ),
            "{refused:?}"
        );
    }
}
