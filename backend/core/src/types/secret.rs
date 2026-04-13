/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Locked-memory secret wrappers.
//!
//! [`SecretString`] and [`SecretBytes`] wrap heap-allocated secrets and:
//!
//! - **Pin to RAM** — call `mlock(2)` on construction so the OS never swaps
//!   the underlying page to disk, even under memory pressure.
//! - **Zeroize on drop** — overwrite every byte with `0` via volatile writes
//!   before the allocator recycles the memory, so secrets are not left in
//!   freed heap regions.
//! - **Redact in output** — `Debug` and `Display` print `[REDACTED]`, making
//!   accidental logging of secrets a no-op.
//!
//! # Usage
//!
//! ```rust,ignore
//! let secret = SecretString::new("my-token".to_string());
//! some_function(secret.expose()); // explicitly opt-in to reading the value
//! // `secret` is zeroed and munlock'd here
//! ```
//!
//! # `mlock` failure handling
//!
//! `mlock` can fail when the process's locked-memory limit (`RLIMIT_MEMLOCK`)
//! is exhausted. We log a warning and continue rather than crashing — the
//! secret is still zeroized on drop; only the swap-prevention guarantee is
//! lost. On Linux you can raise the limit with:
//! ```sh
//! ulimit -l unlimited          # for the current shell
//! systemd: LimitMEMLOCK=512K  # in the service unit
//! ```

use std::fmt;

// ── SecretString ──────────────────────────────────────────────────────────────

/// A heap-allocated string whose memory is locked (non-swappable) and
/// zeroed on drop. Use [`expose`](SecretString::expose) to read the value.
pub struct SecretString(Box<str>);

impl SecretString {
    /// Wrap a `String` as a secret. The underlying memory page is `mlock`ed
    /// immediately; any previous copies of the string (e.g. the original
    /// `String` before conversion) are not locked.
    pub fn new(s: String) -> Self {
        let b = s.into_boxed_str();
        mlock_slice(b.as_bytes());
        Self(b)
    }

    /// Access the secret value. The name `expose` is intentional — it makes
    /// every read-site visible in code review.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
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

// ── SecretBytes ───────────────────────────────────────────────────────────────

/// A heap-allocated byte buffer whose memory is locked (non-swappable) and
/// zeroed on drop. Use [`expose`](SecretBytes::expose) to read the value.
pub struct SecretBytes(Box<[u8]>);

impl SecretBytes {
    /// Wrap a `Vec<u8>` as secret bytes.
    pub fn new(v: Vec<u8>) -> Self {
        let b = v.into_boxed_slice();
        mlock_slice(&b);
        Self(b)
    }

    /// Access the secret bytes.
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

// ── Internals ─────────────────────────────────────────────────────────────────

/// Overwrite every byte with `0` using volatile writes so the compiler cannot
/// optimize the zeroing away (unlike a plain `fill(0)` on a value that is
/// about to be dropped).
fn zeroize_slice(s: &mut [u8]) {
    for byte in s.iter_mut() {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    // Compiler fence: prevent reordering of the volatile stores with
    // subsequent deallocations.
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Attempt to lock the memory pages containing `s` into RAM.
fn mlock_slice(s: &[u8]) {
    if s.is_empty() {
        return;
    }
    #[cfg(unix)]
    {
        let ret = unsafe {
            libc::mlock(s.as_ptr() as *const libc::c_void, s.len())
        };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(
                bytes = s.len(),
                error = %err,
                "mlock failed — secret may be swappable (raise RLIMIT_MEMLOCK or set LimitMEMLOCK in the service unit)"
            );
        }
    }
}

/// Unlock memory pages previously locked by `mlock_slice`.
fn munlock_slice(s: &[u8]) {
    if s.is_empty() {
        return;
    }
    #[cfg(unix)]
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
    fn secret_string_expose() {
        let s = SecretString::new("my-token".to_string());
        assert_eq!(s.expose(), "my-token");
    }

    #[test]
    fn secret_bytes_debug_redacted() {
        let b = SecretBytes::new(vec![1, 2, 3]);
        assert_eq!(format!("{:?}", b), "[REDACTED]");
    }

    #[test]
    fn secret_bytes_expose() {
        let b = SecretBytes::new(vec![10, 20, 30]);
        assert_eq!(b.expose(), &[10u8, 20, 30]);
    }
}
