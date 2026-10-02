/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub fn sign(secret: &[u8], body: &[u8]) -> Option<String> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).ok()?;
    mac.update(body);
    Some(format!(
        "sha256={}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_rfc_4231_case_2() {
        assert_eq!(
            sign(b"Jefe", b"what do ya want for nothing?").as_deref(),
            Some("sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
    }
}
