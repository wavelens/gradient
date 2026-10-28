/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use gradient_secret::SecretBytes;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_encode};

use crate::replacements::Replacements;
use crate::stream::StreamRedactor;

pub const MIN_SECRET_BYTES: usize = 8;

const URL_UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub fn secret_forms(value: &[u8]) -> Vec<SecretBytes> {
    if value.len() < MIN_SECRET_BYTES {
        return Vec::new();
    }

    let trimmed = trim_trailing_newlines(value);
    let mut forms = vec![SecretBytes::from(value), SecretBytes::from(trimmed)];
    forms.extend(lines_of(trimmed));
    for whole in [value, trimmed] {
        forms.extend(encoded_forms(whole));
    }

    forms.retain(|form| form.expose().len() >= MIN_SECRET_BYTES);
    first_of_each(forms, SecretBytes::expose)
}

fn first_of_each<T>(items: Vec<T>, bytes: impl Fn(&T) -> &[u8]) -> Vec<T> {
    let mut seen = HashSet::new();
    let first: Vec<bool> = items.iter().map(|item| seen.insert(bytes(item))).collect();
    items
        .into_iter()
        .zip(first)
        .filter_map(|(item, first)| first.then_some(item))
        .collect()
}

fn trim_trailing_newlines(value: &[u8]) -> &[u8] {
    let end = value
        .iter()
        .rposition(|&b| b != b'\n' && b != b'\r')
        .map_or(0, |i| i + 1);
    &value[..end]
}

fn lines_of(value: &[u8]) -> Vec<SecretBytes> {
    if !value.contains(&b'\n') {
        return Vec::new();
    }

    value
        .split(|&b| b == b'\n')
        .map(|line| SecretBytes::from(line.strip_suffix(b"\r").unwrap_or(line)))
        .collect()
}

fn encoded_forms(value: &[u8]) -> [SecretBytes; 3] {
    [
        SecretBytes::new(STANDARD.encode(value).into_bytes()),
        SecretBytes::new(STANDARD_NO_PAD.encode(value).into_bytes()),
        percent_encoded(value),
    ]
}

fn percent_encoded(value: &[u8]) -> SecretBytes {
    let mut out = Vec::with_capacity(value.len() * 3);
    for part in percent_encode(value, URL_UNRESERVED) {
        out.extend_from_slice(part.as_bytes());
    }
    SecretBytes::new(out)
}

fn form_of(pair: &(SecretBytes, Vec<u8>)) -> &[u8] {
    pair.0.expose()
}

#[derive(Clone)]
pub struct SecretRedactor {
    replacements: Arc<Replacements<SecretBytes>>,
}

impl SecretRedactor {
    pub fn new<'a>(secrets: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Self {
        let pairs = secrets
            .into_iter()
            .flat_map(|(name, value)| {
                let replacement = format!("[secret:{name}]").into_bytes();
                secret_forms(value)
                    .into_iter()
                    .map(move |form| (form, replacement.clone()))
            })
            .collect();
        let mut pairs = first_of_each(pairs, form_of);
        pairs.sort_by_key(|(form, _)| std::cmp::Reverse(form.expose().len()));

        Self {
            replacements: Arc::new(Replacements::new(pairs)),
        }
    }

    pub fn redact(&self, data: &[u8]) -> Vec<u8> {
        self.replacements.apply(data)
    }

    pub fn stream(&self) -> StreamRedactor {
        StreamRedactor::new(Arc::clone(&self.replacements))
    }

    pub fn is_empty(&self) -> bool {
        self.replacements.is_empty()
    }
}

impl Default for SecretRedactor {
    fn default() -> Self {
        Self::new([])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> SecretRedactor {
        SecretRedactor::new([
            ("wandb-token", b"s3cr3t-value".as_slice()),
            ("pin", b"1234".as_slice()),
        ])
    }

    #[test]
    fn the_exact_value_is_masked() {
        assert_eq!(
            redactor().redact(b"token=s3cr3t-value\n"),
            b"token=[secret:wandb-token]\n"
        );
    }

    #[test]
    fn base64_and_url_encoded_forms_are_masked() {
        let r = SecretRedactor::new([("hf-token", b"a b/c+d=e?f".as_slice())]);
        let b64 = STANDARD.encode(b"a b/c+d=e?f");
        assert_eq!(
            r.redact(format!("x {b64} y").as_bytes()),
            b"x [secret:hf-token] y"
        );
        assert_eq!(r.redact(b"q=a%20b%2Fc%2Bd%3De%3Ff"), b"q=[secret:hf-token]");
    }

    #[test]
    fn a_value_under_eight_bytes_is_left_alone_in_every_form() {
        assert_eq!(
            redactor().redact(b"pin 1234 MTIzNA=="),
            b"pin 1234 MTIzNA=="
        );
    }

    #[test]
    fn a_multi_line_value_is_masked_line_by_line() {
        let key = b"-----BEGIN KEY-----\nAAAAB3NzaC1yc2EAAAADAQAB\n-----END KEY-----\n";
        let r = SecretRedactor::new([("deploy-ssh-key", key.as_slice())]);
        assert_eq!(
            r.redact(b"line AAAAB3NzaC1yc2EAAAADAQAB\n"),
            b"line [secret:deploy-ssh-key]\n"
        );
    }
}
