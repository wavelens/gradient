/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;

pub struct SecretString(Box<str>);

impl SecretString {
    /// Only this buffer is locked. Earlier copies of the string, like the original `String`
    /// before conversion, are staying unlocked.
    pub fn new(s: String) -> Self {
        let b = s.into_boxed_str();
        mlock_slice(b.as_bytes());
        Self(b)
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
    pub fn new(v: Vec<u8>) -> Self {
        let b = v.into_boxed_slice();
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

/// Volatile writes are keeping the compiler from optimizing the zeroing away. A plain
/// `fill(0)` on a value about to be dropped is removable.
fn zeroize_slice(s: &mut [u8]) {
    for byte in s.iter_mut() {
        // SAFETY: `byte` is a valid, aligned, exclusively borrowed `&mut u8`. A one-byte volatile
        // write through it is sound.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn mlock_slice(s: &[u8]) {
    if s.is_empty() {
        return;
    }
    #[cfg(unix)]
    {
        // SAFETY: `s` is a non-empty initialized slice. `mlock` is touching only the page range
        // `[ptr, ptr+len)`. Its error code is handled below.
        let ret = unsafe { libc::mlock(s.as_ptr() as *const libc::c_void, s.len()) };
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
    if s.is_empty() {
        return;
    }
    #[cfg(unix)]
    // SAFETY: `s` is a non-empty initialized slice. `munlock` is operating only on the page
    // range `[ptr, ptr+len)`. A failure is benign because the pages are staying locked.
    unsafe {
        libc::munlock(s.as_ptr() as *const libc::c_void, s.len());
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
}
