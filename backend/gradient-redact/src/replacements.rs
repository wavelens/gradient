/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use aho_corasick::{AhoCorasick, MatchKind};

pub struct Replacements<P = Vec<u8>> {
    automaton: Option<AhoCorasick>,
    patterns: Vec<P>,
    replacements: Vec<Vec<u8>>,
    longest: usize,
}

impl<P: AsRef<[u8]>> Replacements<P> {
    pub fn new(pairs: Vec<(P, Vec<u8>)>) -> Self {
        let (patterns, replacements): (Vec<P>, Vec<Vec<u8>>) = pairs
            .into_iter()
            .filter(|(pattern, _)| !pattern.as_ref().is_empty())
            .unzip();
        let longest = patterns
            .iter()
            .map(|pattern| pattern.as_ref().len())
            .max()
            .unwrap_or(0);

        Self {
            automaton: build_automaton(&patterns),
            patterns,
            replacements,
            longest,
        }
    }

    pub fn apply(&self, haystack: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(haystack.len());
        let mut pos = 0;
        for (start, end, index) in self.matches_before(haystack, haystack.len()) {
            out.extend_from_slice(&haystack[pos..start]);
            out.extend_from_slice(&self.replacements[index]);
            pos = end;
        }
        out.extend_from_slice(&haystack[pos..]);
        out
    }

    pub fn apply_str(&self, text: &str) -> String {
        match String::from_utf8(self.apply(text.as_bytes())) {
            Ok(out) => out,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        }
    }

    pub fn matches_before(&self, haystack: &[u8], limit: usize) -> Vec<(usize, usize, usize)> {
        if self.patterns.is_empty() {
            return Vec::new();
        }

        match &self.automaton {
            Some(automaton) => automaton
                .find_iter(haystack)
                .take_while(|m| m.start() < limit)
                .map(|m| (m.start(), m.end(), m.pattern().as_usize()))
                .collect(),
            None => self.scan_matches(haystack, limit),
        }
    }

    pub(crate) fn replacement(&self, index: usize) -> &[u8] {
        &self.replacements[index]
    }

    pub fn longest(&self) -> usize {
        self.longest
    }

    pub fn starts_a_pattern(&self, tail: &[u8]) -> bool {
        !tail.is_empty()
            && self
                .patterns
                .iter()
                .map(|pattern| pattern.as_ref())
                .any(|pattern| pattern.len() > tail.len() && pattern.starts_with(tail))
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    fn scan_matches(&self, haystack: &[u8], limit: usize) -> Vec<(usize, usize, usize)> {
        let mut found = Vec::new();
        let mut start = 0;
        while start < limit.min(haystack.len()) {
            match self.longest_match_at(&haystack[start..]) {
                Some((index, len)) => {
                    found.push((start, start + len, index));
                    start += len;
                }
                None => start += 1,
            }
        }
        found
    }

    fn longest_match_at(&self, rest: &[u8]) -> Option<(usize, usize)> {
        self.patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| (index, pattern.as_ref()))
            .filter(|(_, pattern)| rest.starts_with(pattern))
            .map(|(index, pattern)| (index, pattern.len()))
            .max_by_key(|&(index, len)| (len, std::cmp::Reverse(index)))
    }
}

fn build_automaton<P: AsRef<[u8]>>(patterns: &[P]) -> Option<AhoCorasick> {
    AhoCorasick::builder()
        .match_kind(MatchKind::LeftmostLongest)
        .build(patterns)
        .inspect_err(|e| {
            tracing::warn!(
                error = %e,
                patterns = patterns.len(),
                "redact: pattern matcher did not compile, replacing the slow way",
            );
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_pattern_wins() {
        let r = Replacements::new(vec![
            (b"abc".to_vec(), b"1".to_vec()),
            (b"abcdef".to_vec(), b"2".to_vec()),
        ]);
        assert_eq!(r.apply(b"xabcdefy abc"), b"x2y 1");
    }

    #[test]
    fn a_proper_prefix_starts_a_pattern_and_a_full_match_does_not() {
        let r = Replacements::new(vec![(b"s3cr3t-value".to_vec(), b"[secret:a]".to_vec())]);
        assert!(r.starts_a_pattern(b"s3cr"));
        assert!(!r.starts_a_pattern(b"s3cr3t-value"));
        assert!(!r.starts_a_pattern(b"x"));
    }
}
