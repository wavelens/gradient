/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod page_locks;

use std::fmt;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use page_locks::PageLockCounts;

pub struct SecretString(Box<str>);

impl SecretString {
    pub fn new(s: String) -> Self {
        let b = if s.len() == s.capacity() {
            s.into_boxed_str()
        } else {
            let exact = Box::from(s.as_str());
            zeroize_slice(&mut s.into_bytes());
            exact
        };
        mlock_slice(b.as_bytes());
        Self(b)
    }

    pub fn from_utf8(mut bytes: SecretBytes) -> Option<Self> {
        std::str::from_utf8(bytes.expose()).ok()?;
        let locked = std::mem::take(&mut bytes.0);
        // SAFETY: the bytes were checked to be UTF-8 above. The allocation moves without a copy,
        // so its page locks move with it.
        Some(Self(unsafe { std::str::from_boxed_utf8_unchecked(locked) }))
    }

    pub fn into_bytes(mut self) -> SecretBytes {
        SecretBytes(std::mem::take(&mut self.0).into_boxed_bytes())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        // SAFETY: `Drop` is holding the only reference to `self.0`. The `String` buffer is `len`
        // initialized bytes. One exclusive `&mut [u8]` over it is sound. Nothing is reading the
        // contents afterwards.
        zeroize_slice(unsafe {
            std::slice::from_raw_parts_mut(self.0.as_ptr() as *mut u8, self.0.len())
        });
        munlock_slice(self.0.as_bytes());
    }
}

impl Clone for SecretString {
    fn clone(&self) -> Self {
        Self::new(self.expose().to_owned())
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl From<String> for SecretString {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

pub struct SecretBytes(Box<[u8]>);

impl SecretBytes {
    pub fn new(mut v: Vec<u8>) -> Self {
        let b = if v.len() == v.capacity() {
            v.into_boxed_slice()
        } else {
            let exact = Box::from(v.as_slice());
            zeroize_slice(&mut v);
            exact
        };
        mlock_slice(&b);
        Self(b)
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        zeroize_slice(&mut self.0);
        munlock_slice(&self.0);
    }
}

impl Clone for SecretBytes {
    fn clone(&self) -> Self {
        Self::from(self.expose())
    }
}

impl PartialEq for SecretBytes {
    fn eq(&self, other: &Self) -> bool {
        self.expose() == other.expose()
    }
}

impl Eq for SecretBytes {}

impl AsRef<[u8]> for SecretBytes {
    fn as_ref(&self) -> &[u8] {
        self.expose()
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl From<Vec<u8>> for SecretBytes {
    fn from(v: Vec<u8>) -> Self {
        Self::new(v)
    }
}

impl From<&[u8]> for SecretBytes {
    fn from(bytes: &[u8]) -> Self {
        Self::new(bytes.to_vec())
    }
}

fn zeroize_slice(s: &mut [u8]) {
    for byte in s.iter_mut() {
        // SAFETY: `byte` is a valid, aligned, exclusively borrowed `&mut u8`. A one-byte volatile
        // write through it is sound and the compiler cannot remove it like a plain `fill(0)` on
        // a value about to be dropped.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn page_lock_counts() -> MutexGuard<'static, PageLockCounts> {
    static COUNTS: LazyLock<Mutex<PageLockCounts>> =
        LazyLock::new(|| Mutex::new(PageLockCounts::new(page_size())));
    COUNTS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn page_size() -> usize {
    // SAFETY: `sysconf` is reading a constant system value and has no preconditions.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(size).unwrap_or(4096)
}

fn mlock_slice(s: &[u8]) {
    let mut counts = page_lock_counts();
    for page in counts.lock(s.as_ptr() as usize, s.len()) {
        // SAFETY: `page` is the start of a page holding part of the live allocation `s`.
        // `mlock` is changing only the residency of that page. Its error code is handled below.
        let ret = unsafe { libc::mlock(page as *const libc::c_void, counts.page_size()) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(
                bytes = s.len(),
                error = %err,
                "mlock failed - secret may be swappable (raise RLIMIT_MEMLOCK or set LimitMEMLOCK in the service unit)"
            );
        }
    }
}

fn munlock_slice(s: &[u8]) {
    let mut counts = page_lock_counts();
    for page in counts.unlock(s.as_ptr() as usize, s.len()) {
        // SAFETY: `page` is the start of a page no live secret is using anymore. `munlock` is
        // changing only its residency. A failure is benign because the page is staying locked.
        unsafe {
            libc::munlock(page as *const libc::c_void, counts.page_size());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_string_debug_redacted() {
        let s = SecretString::new("super-secret".to_string());
        assert_eq!(format!("{:?}", s), "[REDACTED]");
    }

    #[test]
    fn secret_string_display_redacted() {
        let s = SecretString::new("super-secret".to_string());
        assert_eq!(format!("{}", s), "[REDACTED]");
    }

    #[test]
    fn secret_bytes_debug_redacted() {
        let b = SecretBytes::new(vec![1, 2, 3]);
        assert_eq!(format!("{:?}", b), "[REDACTED]");
    }

    #[test]
    fn secret_bytes_display_redacted() {
        let b = SecretBytes::new(vec![1, 2, 3]);
        assert_eq!(format!("{}", b), "[REDACTED]");
    }

    #[test]
    fn zeroize_slice_clears_bytes() {
        let mut buf = [1u8, 2, 3, 4];
        zeroize_slice(&mut buf);
        assert_eq!(buf, [0, 0, 0, 0]);
    }

    #[test]
    fn secret_text_converts_to_bytes_and_back_without_losing_content() {
        let bytes = SecretString::new("s3cr3t-value".to_owned()).into_bytes();
        assert_eq!(bytes.expose(), b"s3cr3t-value");

        let text = SecretString::from_utf8(bytes).expect("UTF-8");
        assert_eq!(text.expose(), "s3cr3t-value");
        assert!(SecretString::from_utf8(SecretBytes::from(&[0xff, 0xfe][..])).is_none());
    }

    #[test]
    fn a_buffer_with_spare_capacity_keeps_its_content() {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(b"s3cr3t-value");
        assert_eq!(SecretBytes::new(bytes).expose(), b"s3cr3t-value");

        let mut text = String::with_capacity(64);
        text.push_str("s3cr3t-value");
        assert_eq!(SecretString::new(text).expose(), "s3cr3t-value");
    }
}
