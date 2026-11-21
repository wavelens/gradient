/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    MAX_PULL_REQUEST_PATCH_FILE_BYTES, MAX_PULL_REQUEST_PATCH_FILE_COUNT,
    MAX_PULL_REQUEST_PATCH_WRITTEN_FILES_TOTAL_BYTES, PullRequestError,
};
use flate2::bufread::ZlibDecoder;
use git2::{Delta, Diff, DiffBinaryFile, DiffBinaryKind, DiffDelta, Index, Odb, Repository, Tree};
use gradient_git_host::CommitFile;
use gradient_types::input::check_manifest_path;
use std::io::Read;
use std::path::Path;

pub fn patched_files(
    repo: &Repository,
    start: &Tree<'_>,
    patch: &[u8],
) -> Result<Vec<CommitFile>, PullRequestError> {
    if patch.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let (diff, touched) = parse_patch_and_validate_deltas(patch)?;
    let index = repo
        .apply_to_tree(start, &diff, None)
        .map_err(does_not_apply)?;
    let odb = repo.odb().map_err(does_not_apply)?;
    let mut written_bytes = 0;
    touched
        .into_iter()
        .map(|path| read_commit_file_from_index(repo, &odb, &index, path, &mut written_bytes))
        .collect()
}

pub fn patch_paths(patch: &[u8]) -> Result<Vec<String>, PullRequestError> {
    if patch.iter().all(u8::is_ascii_whitespace) {
        return Err(PullRequestError::Empty);
    }
    parse_patch_and_validate_deltas(patch).map(|(_, touched)| touched)
}

fn parse_patch_and_validate_deltas(
    patch: &[u8],
) -> Result<(Diff<'static>, Vec<String>), PullRequestError> {
    let diff = Diff::from_buffer(patch).map_err(does_not_apply)?;
    let count = diff.deltas().len();
    if count > MAX_PULL_REQUEST_PATCH_FILE_COUNT {
        return Err(PullRequestError::PatchExceedsMaximumFileCount {
            count,
            limit: MAX_PULL_REQUEST_PATCH_FILE_COUNT,
        });
    }
    let touched = diff
        .deltas()
        .map(|delta| written_path(&delta))
        .collect::<Result<Vec<_>, _>>()?;

    let mut refusal = Ok(());
    let mut written_bytes = 0;
    let mut inflated_binary_bytes_total = 0;
    let walked = diff.foreach(
        &mut |_, _| true,
        Some(&mut |delta, binary| {
            let path = path_text(delta.new_file().path());
            let mut inflate = |side: DiffBinaryFile<'_>| {
                inflate_binary_side_with_file_size_cap(
                    &side,
                    &path,
                    &mut inflated_binary_bytes_total,
                )
            };
            refusal = inflate(binary.old_file())
                .and_then(|_| inflate(binary.new_file()))
                .and_then(|size| {
                    written_bytes += size;
                    if written_bytes > MAX_PULL_REQUEST_PATCH_WRITTEN_FILES_TOTAL_BYTES {
                        return Err(total_too_large());
                    }
                    Ok(())
                });
            refusal.is_ok()
        }),
        None,
        None,
    );
    refusal?;
    walked.map_err(does_not_apply)?;
    Ok((diff, touched))
}

fn written_path(delta: &DiffDelta<'_>) -> Result<String, PullRequestError> {
    if matches!(delta.status(), Delta::Deleted | Delta::Renamed) {
        return Err(PullRequestError::DeletesFile(path_text(
            delta.old_file().path(),
        )));
    }
    let new_path = delta.new_file().path();
    let path = new_path
        .and_then(Path::to_str)
        .filter(|path| check_manifest_path(path).is_ok())
        .filter(|path| {
            !path
                .split('/')
                .any(|component| component.eq_ignore_ascii_case(".git"))
        })
        .ok_or_else(|| PullRequestError::PathIsNotRelativeInsideRepository(path_text(new_path)))?;
    Ok(path.to_owned())
}

fn inflate_binary_side_with_file_size_cap(
    side: &DiffBinaryFile<'_>,
    path: &str,
    inflated_binary_bytes_total: &mut usize,
) -> Result<usize, PullRequestError> {
    let declared = side.inflated_len();
    if declared > MAX_PULL_REQUEST_PATCH_FILE_BYTES {
        return Err(file_too_large(path));
    }
    if side.data().is_empty() {
        return Ok(0);
    }
    *inflated_binary_bytes_total += declared;
    if *inflated_binary_bytes_total > MAX_PULL_REQUEST_PATCH_WRITTEN_FILES_TOTAL_BYTES {
        return Err(total_too_large());
    }
    let mut inflated = Vec::with_capacity(declared);
    ZlibDecoder::new(side.data())
        .take(declared as u64 + 1)
        .read_to_end(&mut inflated)
        .map_err(|e| PullRequestError::DoesNotApply(format!("{path}: {e}")))?;
    if inflated.len() != declared {
        return Err(PullRequestError::DoesNotApply(format!(
            "can't decompress the binary data in {path} to the declared length"
        )));
    }
    let size = match side.kind() {
        DiffBinaryKind::Delta => {
            read_target_size_from_delta_header(&inflated).ok_or_else(|| {
                PullRequestError::DoesNotApply(format!(
                    "no valid size header in binary delta {path}"
                ))
            })?
        }
        DiffBinaryKind::Literal | DiffBinaryKind::None => declared,
    };
    if size > MAX_PULL_REQUEST_PATCH_FILE_BYTES {
        return Err(file_too_large(path));
    }
    Ok(size)
}

fn read_target_size_from_delta_header(delta: &[u8]) -> Option<usize> {
    let mut bytes = delta.iter();
    let mut read_size = || {
        let mut size = 0usize;
        for shift in (0..usize::BITS).step_by(7) {
            let byte = *bytes.next()?;
            size |= usize::from(byte & 0x7f).checked_shl(shift)?;
            if byte & 0x80 == 0 {
                return Some(size);
            }
        }
        None
    };
    read_size()?;
    read_size()
}

fn read_commit_file_from_index(
    repo: &Repository,
    odb: &Odb<'_>,
    index: &Index,
    path: String,
    written_bytes: &mut usize,
) -> Result<CommitFile, PullRequestError> {
    let entry = index.get_path(Path::new(&path), 0);
    let mode = entry.as_ref().map_or(0, |entry| entry.mode);
    let Some(entry) = entry.filter(|_| matches!(mode, 0o100644 | 0o100755)) else {
        return Err(PullRequestError::PathIsNotRegularFile { path, mode });
    };
    let (size, _) = odb.read_header(entry.id).map_err(does_not_apply)?;
    if size > MAX_PULL_REQUEST_PATCH_FILE_BYTES {
        return Err(file_too_large(&path));
    }
    *written_bytes += size;
    if *written_bytes > MAX_PULL_REQUEST_PATCH_WRITTEN_FILES_TOTAL_BYTES {
        return Err(total_too_large());
    }
    let contents = repo
        .find_blob(entry.id)
        .map_err(does_not_apply)?
        .content()
        .to_vec();
    Ok(CommitFile { path, contents })
}

fn path_text(path: Option<&Path>) -> String {
    path.map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_too_large(path: &str) -> PullRequestError {
    PullRequestError::PatchExceedsSizeLimit {
        path: path.to_owned(),
        limit: MAX_PULL_REQUEST_PATCH_FILE_BYTES,
    }
}

fn total_too_large() -> PullRequestError {
    PullRequestError::PatchExceedsSizeLimit {
        path: "total of all files".to_owned(),
        limit: MAX_PULL_REQUEST_PATCH_WRITTEN_FILES_TOTAL_BYTES,
    }
}

fn does_not_apply(error: git2::Error) -> PullRequestError {
    PullRequestError::DoesNotApply(error.message().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{DiffOptions, Signature};
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    const MARKER: &[u8] = b"SECRET-MARKER-p02";

    fn repo_with(files: &[(&str, &str)]) -> (TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        for (path, contents) in files {
            std::fs::write(dir.path().join(path), contents).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = Signature::now("t", "t@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
            .unwrap();
        drop(tree);
        (dir, repo)
    }

    fn repo_with_entries(entries: &[(&str, &[u8], i32)]) -> (TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(dir.path()).unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        for (path, contents, mode) in entries {
            builder
                .insert(path, repo.blob(contents).unwrap(), *mode)
                .unwrap();
        }
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let signature = Signature::now("t", "t@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
            .unwrap();
        drop((tree, builder));
        (dir, repo)
    }

    fn head_tree(repo: &Repository) -> Tree<'_> {
        repo.head().unwrap().peel_to_tree().unwrap()
    }

    fn apply(repo: &Repository, patch: &[u8]) -> Result<Vec<CommitFile>, PullRequestError> {
        patched_files(repo, &head_tree(repo), patch)
    }

    fn working_tree_patch(repo: &Repository) -> Vec<u8> {
        let head = head_tree(repo);
        let mut options = DiffOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true)
            .show_binary(true);
        let diff = repo
            .diff_tree_to_workdir(Some(&head), Some(&mut options))
            .unwrap();
        let mut patch = Vec::new();
        diff.print(git2::DiffFormat::Patch, |_, _, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                patch.push(line.origin() as u8);
            }
            patch.extend_from_slice(line.content());
            true
        })
        .unwrap();
        patch
    }

    fn assert_no_marker(result: &Result<Vec<CommitFile>, PullRequestError>) {
        if let Ok(written) = result {
            assert!(
                written
                    .iter()
                    .all(|file| !file.contents.windows(MARKER.len()).any(|w| w == MARKER)),
                "secret leaked: {written:?}"
            );
        }
    }

    fn symlink_to_secret_patch() -> (TempDir, Vec<u8>) {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret");
        std::fs::write(&secret, MARKER).unwrap();
        let (edited, repo) = repo_with(&[("README", "r\n")]);
        std::os::unix::fs::symlink(&secret, edited.path().join("leak")).unwrap();
        (outside, working_tree_patch(&repo))
    }

    fn assert_not_regular(
        result: &Result<Vec<CommitFile>, PullRequestError>,
        expected_path: &str,
        expected_mode: u32,
    ) {
        assert!(
            matches!(
                result,
                Err(PullRequestError::PathIsNotRegularFile { path, mode })
                    if path == expected_path && *mode == expected_mode
            ),
            "{result:?}"
        );
    }

    #[test]
    fn a_patch_yields_the_new_contents_of_each_touched_file() {
        let files = [("flake.lock", "a\n"), ("README", "r\n")];
        let (_base_dir, base) = repo_with(&files);
        let (edited, repo) = repo_with(&files);
        std::fs::write(edited.path().join("flake.lock"), "b\n").unwrap();
        std::fs::write(edited.path().join("new.txt"), "new\n").unwrap();

        let mut written = apply(&base, &working_tree_patch(&repo)).unwrap();
        written.sort_by(|a, b| a.path.cmp(&b.path));

        assert_eq!(
            written,
            vec![
                CommitFile {
                    path: "flake.lock".into(),
                    contents: b"b\n".to_vec()
                },
                CommitFile {
                    path: "new.txt".into(),
                    contents: b"new\n".to_vec()
                },
            ]
        );
    }

    #[test]
    fn a_patch_that_deletes_a_file_is_refused_with_its_path() {
        let files = [("flake.lock", "a\n"), ("README", "r\n")];
        let (_base_dir, base) = repo_with(&files);
        let (edited, repo) = repo_with(&files);
        std::fs::remove_file(edited.path().join("README")).unwrap();

        let refused = apply(&base, &working_tree_patch(&repo));

        assert!(
            matches!(&refused, Err(PullRequestError::DeletesFile(path)) if path == "README"),
            "{refused:?}"
        );
    }

    #[test]
    fn a_symlink_to_a_file_outside_the_repository_is_refused() {
        let (_outside, patch) = symlink_to_secret_patch();
        let (_base_dir, base) = repo_with(&[("README", "r\n")]);

        let result = apply(&base, &patch);

        assert_no_marker(&result);
        assert_not_regular(&result, "leak", 0o120000);
    }

    #[test]
    fn a_symlink_is_refused_on_a_bare_repository_fetched_from_an_origin() {
        let (_outside, patch) = symlink_to_secret_patch();
        let (base_dir, base) = repo_with(&[("README", "r\n")]);
        let start = base.head().unwrap().target().unwrap().to_string();
        let url = gradient_types::RepositoryUrl::local_test_repository(base_dir.path());

        let fetched = gradient_sources::fetch_commit_into_new_bare_temporary_repository(
            &url,
            &start,
            &gradient_sources::GitOperationCredentials::default(),
        )
        .unwrap();
        let result = patched_files(
            fetched.repository(),
            &fetched.fetched_commit_tree().unwrap(),
            &patch,
        );

        assert!(fetched.repository().workdir().is_none());
        assert_no_marker(&result);
        assert_not_regular(&result, "leak", 0o120000);
    }

    fn symlink_edit_patch(index_line: &str) -> String {
        format!(
            "diff --git a/link b/link\n\
{index_line}\
--- a/link\n\
+++ b/link\n\
@@ -1 +1 @@\n\
-/etc/hostname\n\
\\ No newline at end of file\n\
+/etc/passwd\n\
\\ No newline at end of file\n"
        )
    }

    #[test]
    fn an_edit_of_an_existing_symlink_is_refused() {
        let (_dir, base) = repo_with_entries(&[("link", b"/etc/hostname", 0o120000)]);
        let old = git2::Oid::hash_object(git2::ObjectType::Blob, b"/etc/hostname").unwrap();
        let new = git2::Oid::hash_object(git2::ObjectType::Blob, b"/etc/passwd").unwrap();
        let patch = symlink_edit_patch(&format!("index {old}..{new} 120000\n"));

        let result = apply(&base, patch.as_bytes());

        assert_not_regular(&result, "link", 0o120000);
    }

    #[test]
    fn a_symlink_edit_without_mode_lines_yields_only_the_literal_patch_text() {
        let (_dir, base) = repo_with_entries(&[("link", b"/etc/hostname", 0o120000)]);

        let result = apply(&base, symlink_edit_patch("").as_bytes());

        assert_eq!(
            result.unwrap(),
            vec![CommitFile {
                path: "link".into(),
                contents: b"/etc/passwd".to_vec()
            }]
        );
    }

    #[test]
    fn a_new_submodule_entry_is_refused() {
        let (_dir, base) = repo_with_entries(&[("README", b"r\n", 0o100644)]);
        let sha = "1111111111111111111111111111111111111111";
        let patch = format!(
            "diff --git a/sub b/sub\n\
new file mode 160000\n\
index 0000000..1111111\n\
--- /dev/null\n\
+++ b/sub\n\
@@ -0,0 +1 @@\n\
+Subproject commit {sha}\n"
        );

        let result = apply(&base, patch.as_bytes());

        assert_not_regular(&result, "sub", 0o160000);
    }

    #[test]
    fn a_binary_file_over_the_size_limit_is_refused_before_apply() {
        let (_base_dir, base) = repo_with(&[("README", "r\n")]);
        let (edited, repo) = repo_with(&[("README", "r\n")]);
        std::fs::write(
            edited.path().join("bomb"),
            vec![0u8; MAX_PULL_REQUEST_PATCH_FILE_BYTES + 1],
        )
        .unwrap();
        let patch = working_tree_patch(&repo);

        let result = apply(&base, &patch);

        assert!(
            matches!(&result, Err(PullRequestError::PatchExceedsSizeLimit { path, .. }) if path == "bomb"),
            "{result:?}"
        );
        assert!(
            matches!(
                patch_paths(&patch),
                Err(PullRequestError::PatchExceedsSizeLimit { .. })
            ),
            "patch_paths must refuse the same patch"
        );
    }

    #[test]
    fn a_binary_file_that_inflates_beyond_its_declared_length_is_refused() {
        let (_base_dir, base) = repo_with(&[("README", "r\n")]);
        let (edited, repo) = repo_with(&[("README", "r\n")]);
        let real_length = MAX_PULL_REQUEST_PATCH_FILE_BYTES + 1;
        std::fs::write(edited.path().join("bomb"), vec![0u8; real_length]).unwrap();
        let patch = String::from_utf8(working_tree_patch(&repo)).unwrap();
        let lying = patch.replacen(&format!("literal {real_length}"), "literal 10", 1);
        assert_ne!(patch, lying);

        let result = apply(&base, lying.as_bytes());

        assert!(
            matches!(&result, Err(PullRequestError::DoesNotApply(_))),
            "{result:?}"
        );
    }

    fn encode_git_binary_patch_line(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 85] =
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";
        let length = match u8::try_from(bytes.len()).unwrap() {
            n @ 1..=26 => b'A' + n - 1,
            n => b'a' + n - 27,
        };
        let mut line = String::from(length as char);
        for chunk in bytes.chunks(4) {
            let mut group = [0u8; 4];
            group[..chunk.len()].copy_from_slice(chunk);
            let mut value = u32::from_be_bytes(group);
            let mut digits = [0u8; 5];
            for digit in digits.iter_mut().rev() {
                *digit = ALPHABET[(value % 85) as usize];
                value /= 85;
            }
            line.extend(digits.iter().map(|&d| d as char));
        }
        line
    }

    #[test]
    fn a_git_binary_patch_whose_delta_header_target_size_exceeds_the_per_file_size_limit_is_refused_before_apply()
     {
        use std::io::Write;
        let (_dir, base) = repo_with_entries(&[("blob", &[0u8; 16], 0o100644)]);
        let mut delta = vec![16u8];
        let mut target = MAX_PULL_REQUEST_PATCH_FILE_BYTES + 1;
        while target >= 0x80 {
            delta.push(u8::try_from(target & 0x7f).unwrap() | 0x80);
            target >>= 7;
        }
        delta.extend([u8::try_from(target).unwrap(), 0x01, 0x00]);
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(&delta).unwrap();
        let compressed = encoder.finish().unwrap();
        let lines: Vec<String> = compressed
            .chunks(52)
            .map(encode_git_binary_patch_line)
            .collect();
        let patch = format!(
            "diff --git a/blob b/blob\nindex {}..{} 100644\nGIT binary patch\ndelta {}\n{}\n\nliteral 0\nHcmV?d00001\n\n",
            git2::Oid::hash_object(git2::ObjectType::Blob, &[0u8; 16]).unwrap(),
            git2::Oid::hash_object(git2::ObjectType::Blob, b"target").unwrap(),
            delta.len(),
            lines.join("\n")
        );

        let result = apply(&base, patch.as_bytes());

        assert!(
            matches!(&result, Err(PullRequestError::PatchExceedsSizeLimit { path, limit })
                if path == "blob" && *limit == MAX_PULL_REQUEST_PATCH_FILE_BYTES),
            "{result:?}"
        );
    }

    #[test]
    fn a_patch_whose_patched_files_exceed_the_total_size_limit_is_refused() {
        let file_count = 5;
        let mut large = b"a\n".to_vec();
        large.resize(
            MAX_PULL_REQUEST_PATCH_WRITTEN_FILES_TOTAL_BYTES / file_count + 1,
            b'x',
        );
        large.push(b'\n');
        assert!(large.len() < MAX_PULL_REQUEST_PATCH_FILE_BYTES);
        let names: Vec<String> = (0..file_count).map(|n| format!("f{n}")).collect();
        let entries: Vec<(&str, &[u8], i32)> = names
            .iter()
            .map(|name| (name.as_str(), large.as_slice(), 0o100644))
            .collect();
        let (_dir, base) = repo_with_entries(&entries);
        let patch: String = names
            .iter()
            .map(|n| format!("diff --git a/{n} b/{n}\n--- a/{n}\n+++ b/{n}\n@@ -1 +1 @@\n-a\n+b\n"))
            .collect();

        let result = apply(&base, patch.as_bytes());

        assert!(
            matches!(&result, Err(PullRequestError::PatchExceedsSizeLimit { limit, .. })
                if *limit == MAX_PULL_REQUEST_PATCH_WRITTEN_FILES_TOTAL_BYTES),
            "{result:?}"
        );
    }

    fn encode_git_binary_patch_block(bytes: &[u8]) -> String {
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(bytes).unwrap();
        let compressed = encoder.finish().unwrap();
        let lines: Vec<String> = compressed
            .chunks(52)
            .map(encode_git_binary_patch_line)
            .collect();
        format!("literal {}\n{}\n\n", bytes.len(), lines.join("\n"))
    }

    #[test]
    fn refuses_git_binary_patch_with_many_hunks_before_decompression_when_summed_old_and_new_sides_exceed_32_mib()
     {
        let (_dir, base) = repo_with_entries(&[("README", b"r\n", 0o100644)]);
        let new_side = encode_git_binary_patch_block(b"x");
        let old_side = encode_git_binary_patch_block(&vec![0u8; MAX_PULL_REQUEST_PATCH_FILE_BYTES]);
        let file_count = MAX_PULL_REQUEST_PATCH_WRITTEN_FILES_TOTAL_BYTES
            / MAX_PULL_REQUEST_PATCH_FILE_BYTES
            + 1;
        let patch: String = (0..file_count)
            .map(|n| {
                format!(
                    "diff --git a/b{n} b/b{n}\nindex 1111111..2222222 100644\nGIT binary patch\n{new_side}{old_side}"
                )
            })
            .collect();
        assert!(patch.len() < crate::pull_requests::MAX_PULL_REQUEST_PATCH_BYTES);

        let applied = apply(&base, patch.as_bytes());
        let named = patch_paths(patch.as_bytes());

        for result in [applied.map(|_| ()), named.map(|_| ())] {
            assert!(
                matches!(&result, Err(PullRequestError::PatchExceedsSizeLimit { limit, .. })
                    if *limit == MAX_PULL_REQUEST_PATCH_WRITTEN_FILES_TOTAL_BYTES),
                "{result:?}"
            );
        }
    }

    #[test]
    fn a_symlink_copy_with_mode_lines_is_refused_and_without_mode_lines_yields_the_link_target_as_a_regular_file()
     {
        let (_dir, base) = repo_with_entries(&[("link", b"/etc/hostname", 0o120000)]);
        let copy = |mode_lines: &str| {
            let patch = format!(
                "diff --git a/link b/copy\n{mode_lines}similarity index 100%\ncopy from link\ncopy to copy\n"
            );
            apply(&base, patch.as_bytes())
        };

        let with_mode_lines = copy("old mode 120000\nnew mode 120000\n");
        let without_mode_lines = copy("");

        assert_not_regular(&with_mode_lines, "copy", 0o120000);
        assert_eq!(
            without_mode_lines.unwrap(),
            vec![CommitFile {
                path: "copy".into(),
                contents: b"/etc/hostname".to_vec()
            }]
        );
    }

    #[test]
    fn refuses_patch_that_only_changes_regular_file_mode_to_symlink_or_submodule() {
        let (_dir, base) = repo_with_entries(&[("file", b"/etc/hostname", 0o100644)]);
        for mode in [0o120000, 0o160000] {
            let patch = format!("diff --git a/file b/file\nold mode 100644\nnew mode {mode:o}\n");

            let result = apply(&base, patch.as_bytes());

            assert_not_regular(&result, "file", mode);
        }
    }

    #[test]
    fn a_small_binary_file_is_accepted() {
        let (_base_dir, base) = repo_with(&[("README", "r\n")]);
        let (edited, repo) = repo_with(&[("README", "r\n")]);
        std::fs::write(edited.path().join("image"), [0u8, 1, 2, 3, 0, 255]).unwrap();

        let written = apply(&base, &working_tree_patch(&repo)).unwrap();

        assert_eq!(
            written,
            vec![CommitFile {
                path: "image".into(),
                contents: vec![0, 1, 2, 3, 0, 255]
            }]
        );
    }

    fn build_patch_text_adding_new_files(count: usize) -> String {
        (0..count)
            .map(|n| {
                format!(
                    "diff --git a/f{n} b/f{n}\nnew file mode 100644\n--- /dev/null\n+++ b/f{n}\n@@ -0,0 +1 @@\n+x\n"
                )
            })
            .collect()
    }

    #[test]
    fn a_patch_with_exactly_the_maximum_allowed_file_count_is_accepted() {
        let (_dir, base) = repo_with_entries(&[("README", b"r\n", 0o100644)]);
        let patch = build_patch_text_adding_new_files(MAX_PULL_REQUEST_PATCH_FILE_COUNT);

        let written = apply(&base, patch.as_bytes()).unwrap();

        assert_eq!(written.len(), MAX_PULL_REQUEST_PATCH_FILE_COUNT);
        assert!(written.iter().all(|file| file.contents == b"x\n"));
    }

    #[test]
    fn a_patch_with_too_many_files_is_refused() {
        let (_dir, base) = repo_with_entries(&[("README", b"r\n", 0o100644)]);
        let patch = build_patch_text_adding_new_files(MAX_PULL_REQUEST_PATCH_FILE_COUNT + 1);

        let result = apply(&base, patch.as_bytes());

        assert!(
            matches!(
                &result,
                Err(PullRequestError::PatchExceedsMaximumFileCount { count, .. })
                    if *count == MAX_PULL_REQUEST_PATCH_FILE_COUNT + 1
            ),
            "{result:?}"
        );
    }

    #[test]
    fn paths_outside_the_repository_or_inside_git_metadata_are_refused() {
        let (_dir, base) = repo_with_entries(&[("README", b"r\n", 0o100644)]);
        for path in ["../escape", "sub/.git/config", ".GIT/hooks/pre-commit"] {
            let patch = format!(
                "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+x\n"
            );

            let applied = apply(&base, patch.as_bytes());
            let named = patch_paths(patch.as_bytes());

            for result in [applied.map(|_| ()), named.map(|_| ())] {
                assert!(
                    matches!(
                        &result,
                        Err(PullRequestError::PathIsNotRelativeInsideRepository(refused))
                            if refused == path
                    ),
                    "{path}: {result:?}"
                );
            }
        }
    }

    #[test]
    fn a_new_executable_file_is_accepted() {
        let (_base_dir, base) = repo_with(&[("README", "r\n")]);
        let (edited, repo) = repo_with(&[("README", "r\n")]);
        let script = edited.path().join("run.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let written = apply(&base, &working_tree_patch(&repo)).unwrap();

        assert_eq!(
            written,
            vec![CommitFile {
                path: "run.sh".into(),
                contents: b"#!/bin/sh\n".to_vec()
            }]
        );
    }

    #[test]
    fn a_patch_for_another_tree_does_not_apply() {
        let (_base_dir, base) = repo_with(&[("flake.lock", "a\n")]);
        let (edited, repo) = repo_with(&[("flake.lock", "zzz\n")]);
        std::fs::write(edited.path().join("flake.lock"), "b\n").unwrap();

        let refused = apply(&base, &working_tree_patch(&repo));

        assert!(
            matches!(refused, Err(PullRequestError::DoesNotApply(_))),
            "{refused:?}"
        );
    }
}
