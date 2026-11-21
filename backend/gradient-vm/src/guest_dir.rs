/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path};

use anyhow::{Context, Result, ensure};
use rustix::fs::{FileType, Mode, OFlags};
use rustix::io::Errno;
use tokio::net::UnixStream;

pub(crate) struct OpenedGuestDirectory {
    directory_file_descriptor: OwnedFd,
    guest_directory_owner_user_id: u32,
    worker_group_id: u32,
}

impl OpenedGuestDirectory {
    pub(crate) fn open_guest_directory_without_symlink_and_reject_root_owner(
        dir: &Path,
    ) -> Result<Self> {
        ensure!(
            dir.is_absolute()
                && !dir
                    .components()
                    .any(|component| component == Component::ParentDir),
            "Guest directory {} is not an absolute path without a `..` component.",
            dir.display()
        );
        let directory_file_descriptor = open_absolute_directory_without_following_symlinks(dir)
            .with_context(|| format!("open {} without following symlinks", dir.display()))?;
        let owner = rustix::fs::fstat(&directory_file_descriptor)
            .with_context(|| format!("Read the owner of {}", dir.display()))?;
        error_if_guest_directory_owner_is_root(owner.st_uid)?;
        Ok(Self {
            directory_file_descriptor,
            guest_directory_owner_user_id: owner.st_uid,
            worker_group_id: owner.st_gid,
        })
    }

    pub(crate) fn guest_directory_owner_user_id(&self) -> u32 {
        self.guest_directory_owner_user_id
    }

    pub(crate) fn worker_group_id(&self) -> u32 {
        self.worker_group_id
    }

    pub(crate) async fn connect_to_worker_owned_socket_in_guest_directory_without_following_symlinks(
        &self,
        name: &str,
    ) -> Result<UnixStream> {
        let socket = rustix::fs::openat(
            self,
            name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| {
            format!("Open {name} in the guest directory without following symlinks")
        })?;
        let socket_stat = rustix::fs::fstat(&socket)
            .with_context(|| format!("Read the type and owner of {name}"))?;
        error_if_file_is_not_worker_owned_socket(
            FileType::from_raw_mode(socket_stat.st_mode),
            socket_stat.st_uid,
            self.guest_directory_owner_user_id,
        )?;
        UnixStream::connect(format!("/proc/self/fd/{}", socket.as_raw_fd()))
            .await
            .with_context(|| format!("Connect to {name}"))
    }
}

fn open_absolute_directory_without_following_symlinks(dir: &Path) -> rustix::io::Result<OwnedFd> {
    let path_component_open_flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut components = dir.components();
    if components.next() != Some(Component::RootDir) {
        return Err(Errno::INVAL);
    }
    components.try_fold(
        rustix::fs::open("/", path_component_open_flags, Mode::empty())?,
        |parent, component| match component {
            Component::Normal(name) => {
                rustix::fs::openat(&parent, name, path_component_open_flags, Mode::empty())
            }
            _ => Err(Errno::INVAL),
        },
    )
}

fn error_if_file_is_not_worker_owned_socket(
    file_type: FileType,
    owner_user_id: u32,
    worker_user_id: u32,
) -> Result<()> {
    ensure!(
        file_type == FileType::Socket && owner_user_id == worker_user_id,
        "file is not a socket owned by the worker user"
    );
    Ok(())
}

impl AsFd for OpenedGuestDirectory {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.directory_file_descriptor.as_fd()
    }
}

fn error_if_guest_directory_owner_is_root(owner_user_id: u32) -> Result<()> {
    ensure!(
        owner_user_id != 0,
        "guest directory owner is root, not worker user"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;
    use crate::console::SERIAL_SOCKET;

    impl OpenedGuestDirectory {
        fn inode_number_of_opened_directory(&self) -> u64 {
            rustix::fs::fstat(self).unwrap().st_ino
        }
    }

    #[test]
    fn opened_guest_directory_keeps_original_inode_when_path_is_replaced_by_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let guest = root.join("guest");
        let symlink_target_directory_path = root.join("target");
        std::fs::create_dir(&guest).unwrap();
        std::fs::create_dir(&symlink_target_directory_path).unwrap();
        let original_guest_directory_inode_number = std::fs::metadata(&guest).unwrap().ino();

        let opened_guest_directory =
            OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner(
                &guest,
            )
            .unwrap();
        std::fs::rename(&guest, root.join("moved")).unwrap();
        std::os::unix::fs::symlink(&symlink_target_directory_path, &guest).unwrap();

        assert_eq!(
            opened_guest_directory.inode_number_of_opened_directory(),
            original_guest_directory_inode_number
        );
        assert_ne!(
            opened_guest_directory.inode_number_of_opened_directory(),
            std::fs::metadata(&symlink_target_directory_path)
                .unwrap()
                .ino()
        );
        assert_eq!(
            opened_guest_directory.guest_directory_owner_user_id(),
            std::fs::metadata(root.join("moved")).unwrap().uid()
        );
    }

    #[test]
    fn symlinked_guest_directory_is_rejected_while_real_directory_is_accepted() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let real = root.join("guest");
        let link = root.join("link");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::fs::write(root.join("file"), b"").unwrap();
        let open_guest_directory_without_symlink_and_reject_root_owner =
            OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner;

        assert!(open_guest_directory_without_symlink_and_reject_root_owner(&real).is_ok());
        assert!(open_guest_directory_without_symlink_and_reject_root_owner(&link).is_err());
        assert!(
            open_guest_directory_without_symlink_and_reject_root_owner(
                &link.join("..").join("guest")
            )
            .is_err()
        );
        assert!(
            open_guest_directory_without_symlink_and_reject_root_owner(&root.join("missing"))
                .is_err()
        );
        assert!(
            open_guest_directory_without_symlink_and_reject_root_owner(&root.join("file")).is_err()
        );
    }

    #[test]
    fn open_guest_dir_refuses_symlinked_ancestor_below_worker_base_dir() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let guests_directory_below_worker_base = root.join("guests");
        let symlink_target_guest_dir_with_same_id = root.join("other").join("guest");
        std::fs::create_dir_all(&symlink_target_guest_dir_with_same_id).unwrap();
        std::os::unix::fs::symlink(root.join("other"), &guests_directory_below_worker_base)
            .unwrap();

        assert!(
            OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner(
                &guests_directory_below_worker_base.join("guest")
            )
            .is_err()
        );
    }

    #[test]
    fn open_guest_directory_returns_file_descriptor_matching_path_device_and_inode() {
        let temp = tempfile::tempdir().unwrap();
        let guest = std::fs::canonicalize(temp.path()).unwrap().join("guest");
        std::fs::create_dir(&guest).unwrap();

        let opened = rustix::fs::fstat(
            OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner(
                &guest,
            )
            .unwrap(),
        )
        .unwrap();
        let metadata = std::fs::metadata(&guest).unwrap();

        assert_eq!(
            (opened.st_dev as u64, opened.st_ino as u64),
            (metadata.dev(), metadata.ino())
        );
    }

    #[test]
    fn open_absolute_directory_without_following_symlinks_rejects_relative_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let existing_absolute_temp_directory_path_without_leading_slash =
            root.strip_prefix("/").unwrap();

        assert!(
            Path::new("/")
                .join(existing_absolute_temp_directory_path_without_leading_slash)
                .is_dir()
        );
        assert!(
            open_absolute_directory_without_following_symlinks(
                existing_absolute_temp_directory_path_without_leading_slash
            )
            .is_err()
        );
    }

    #[test]
    fn open_absolute_directory_without_following_symlinks_rejects_parent_directory_component() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let nested_guest_directory_that_dropped_parent_component_would_reach =
            root.join("nested").join("guest");
        std::fs::create_dir_all(&nested_guest_directory_that_dropped_parent_component_would_reach)
            .unwrap();
        std::fs::create_dir(root.join("guest")).unwrap();
        let absolute_path_with_parent_directory_component =
            root.join("nested").join("..").join("guest");

        assert!(nested_guest_directory_that_dropped_parent_component_would_reach.is_dir());
        assert!(
            open_absolute_directory_without_following_symlinks(
                &absolute_path_with_parent_directory_component
            )
            .is_err()
        );
    }

    #[test]
    fn test_open_absolute_directory_without_following_symlinks_accepts_dot_components_and_repeated_slashes()
     {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let guest = root.join("guest");
        std::fs::create_dir(&guest).unwrap();

        let opened = rustix::fs::fstat(
            open_absolute_directory_without_following_symlinks(Path::new(&format!(
                "//{}/.//guest//",
                root.display()
            )))
            .unwrap(),
        )
        .unwrap();
        let metadata = std::fs::metadata(&guest).unwrap();

        assert_eq!(
            (opened.st_dev as u64, opened.st_ino as u64),
            (metadata.dev(), metadata.ino())
        );
    }

    #[test]
    fn opens_guest_directory_for_redundant_slashes_and_dot_components_and_rejects_relative_and_parent_paths_without_symlink()
     {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let guest = root.join("guest");
        std::fs::create_dir(&guest).unwrap();
        let guest_dir_path_with_double_slash_dot_and_trailing_slash =
            std::path::PathBuf::from(format!("/{}//./guest/", root.display()));
        let open_guest_directory_without_symlink_and_reject_root_owner =
            OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner;

        assert_eq!(
            open_guest_directory_without_symlink_and_reject_root_owner(
                &guest_dir_path_with_double_slash_dot_and_trailing_slash
            )
            .unwrap()
            .inode_number_of_opened_directory(),
            std::fs::metadata(&guest).unwrap().ino()
        );
        assert!(
            open_guest_directory_without_symlink_and_reject_root_owner(Path::new("guest")).is_err()
        );
        assert!(
            open_guest_directory_without_symlink_and_reject_root_owner(
                &guest.join("..").join("guest")
            )
            .is_err()
        );
    }

    #[test]
    fn test_rejects_proc_cwd_magic_link_as_guest_directory() {
        let test_process_current_working_directory_proc_link_path = Path::new("/proc")
            .join(std::process::id().to_string())
            .join("cwd");

        assert!(test_process_current_working_directory_proc_link_path.is_dir());
        assert!(
            OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner(
                &test_process_current_working_directory_proc_link_path
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn socket_symlink_to_outside_listener_is_refused_and_receives_no_connection() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let guest = root.join("guest");
        std::fs::create_dir(&guest).unwrap();
        let unix_socket_path_outside_guest_dir = root.join("outside.sock");
        let listener_bound_outside_guest_directory =
            tokio::net::UnixListener::bind(&unix_socket_path_outside_guest_dir).unwrap();
        std::os::unix::fs::symlink(
            &unix_socket_path_outside_guest_dir,
            guest.join(SERIAL_SOCKET),
        )
        .unwrap();
        let opened_guest_directory =
            OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner(
                &guest,
            )
            .unwrap();

        assert!(
            opened_guest_directory
                .connect_to_worker_owned_socket_in_guest_directory_without_following_symlinks(
                    SERIAL_SOCKET
                )
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                listener_bound_outside_guest_directory.accept()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn connect_to_socket_replaced_by_symlink_after_guest_dir_open_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let guest = root.join("guest");
        std::fs::create_dir(&guest).unwrap();
        let unix_socket_path_outside_guest_dir = root.join("outside.sock");
        let _listener_bound_outside_guest_directory =
            tokio::net::UnixListener::bind(&unix_socket_path_outside_guest_dir).unwrap();
        let _listener_bound_on_real_socket_file_in_guest_directory =
            tokio::net::UnixListener::bind(guest.join(SERIAL_SOCKET)).unwrap();
        let opened_guest_directory =
            OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner(
                &guest,
            )
            .unwrap();
        assert!(
            opened_guest_directory
                .connect_to_worker_owned_socket_in_guest_directory_without_following_symlinks(
                    SERIAL_SOCKET
                )
                .await
                .is_ok()
        );

        std::fs::remove_file(guest.join(SERIAL_SOCKET)).unwrap();
        std::os::unix::fs::symlink(
            &unix_socket_path_outside_guest_dir,
            guest.join(SERIAL_SOCKET),
        )
        .unwrap();

        assert!(
            opened_guest_directory
                .connect_to_worker_owned_socket_in_guest_directory_without_following_symlinks(
                    SERIAL_SOCKET
                )
                .await
                .is_err()
        );
    }

    #[test]
    fn test_pure_check_accepts_socket_with_worker_uid_and_rejects_other_uid_symlink_or_regular_file()
     {
        assert!(error_if_file_is_not_worker_owned_socket(FileType::Socket, 1000, 1000).is_ok());
        assert!(error_if_file_is_not_worker_owned_socket(FileType::Socket, 0, 1000).is_err());
        assert!(error_if_file_is_not_worker_owned_socket(FileType::Symlink, 1000, 1000).is_err());
        assert!(
            error_if_file_is_not_worker_owned_socket(FileType::RegularFile, 1000, 1000).is_err()
        );
    }

    #[test]
    fn guest_directory_refuses_root_owner_uid_and_accepts_non_root_owner_uid() {
        assert!(error_if_guest_directory_owner_is_root(0).is_err());
        assert!(error_if_guest_directory_owner_is_root(1000).is_ok());
    }
}
