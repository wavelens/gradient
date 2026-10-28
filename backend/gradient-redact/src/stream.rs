/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use crate::replacements::Replacements;

pub struct StreamRedactor {
    replacements: Arc<Replacements>,
    held_back: Vec<u8>,
}

impl StreamRedactor {
    pub(crate) fn new(replacements: Arc<Replacements>) -> Self {
        Self {
            replacements,
            held_back: Vec::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut buf = std::mem::take(&mut self.held_back);
        buf.extend_from_slice(chunk);
        let hold = self.start_of_unfinished_secret(&buf);

        let mut out = Vec::with_capacity(buf.len());
        let mut pos = 0;
        for (start, end, index) in self.replacements.matches_before(&buf, hold) {
            out.extend_from_slice(&buf[pos..start]);
            out.extend_from_slice(self.replacements.replacement(index));
            pos = end;
        }

        let emit_end = pos.max(hold);
        out.extend_from_slice(&buf[pos..emit_end]);
        self.held_back = buf.split_off(emit_end);
        out
    }

    pub fn finish(&mut self) -> Vec<u8> {
        self.replacements
            .apply(&std::mem::take(&mut self.held_back))
    }

    fn start_of_unfinished_secret(&self, buf: &[u8]) -> usize {
        let earliest = buf
            .len()
            .saturating_sub(self.replacements.longest().saturating_sub(1));
        (earliest..buf.len())
            .find(|&start| self.replacements.starts_a_pattern(&buf[start..]))
            .unwrap_or(buf.len())
    }
}

#[cfg(test)]
mod tests {
    use crate::SecretRedactor;

    fn redactor() -> SecretRedactor {
        SecretRedactor::new([
            ("wandb-token", b"s3cr3t-value".as_slice()),
            ("pin", b"1234".as_slice()),
        ])
    }

    fn collect(r: &SecretRedactor, chunks: &[&[u8]]) -> Vec<u8> {
        let mut s = r.stream();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(s.push(c));
        }
        out.extend(s.finish());
        out
    }

    #[test]
    fn a_secret_split_across_two_chunks_is_masked() {
        assert_eq!(
            collect(&redactor(), &[b"token=s3cr3t", b"-value done"]),
            b"token=[secret:wandb-token] done"
        );
    }

    #[test]
    fn a_secret_split_byte_by_byte_is_masked() {
        let input = b"a s3cr3t-value b";
        let chunks: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(collect(&redactor(), &chunks), b"a [secret:wandb-token] b");
    }

    #[test]
    fn text_that_cannot_start_a_secret_is_not_held_back() {
        let mut s = redactor().stream();
        assert_eq!(s.push(b"hello "), b"hello ");
        assert_eq!(s.push(b"s3c"), b"");
    }

    #[test]
    fn a_longer_secret_sharing_a_prefix_wins_across_chunks() {
        let r = SecretRedactor::new([
            ("short", b"password1".as_slice()),
            ("long", b"password12345".as_slice()),
        ]);
        assert_eq!(collect(&r, &[b"xpassword1", b"2345y"]), b"x[secret:long]y");
        assert_eq!(collect(&r, &[b"xpassword1", b" y"]), b"x[secret:short] y");
    }

    #[test]
    fn finish_releases_an_unfinished_prefix() {
        assert_eq!(collect(&redactor(), &[b"s3cr3t-val"]), b"s3cr3t-val");
    }
}
