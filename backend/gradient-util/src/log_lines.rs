/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Debug, Default)]
pub struct PrefixedLines {
    prefix: String,
    pending: String,
}

impl PrefixedLines {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            pending: String::new(),
        }
    }

    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.pending.push_str(chunk);
        let Some(end) = self.pending.rfind('\n') else {
            return Vec::new();
        };

        let complete: String = self.pending.drain(..=end).collect();
        complete.lines().map(|line| self.prefixed(line)).collect()
    }

    pub fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }

        let rest = std::mem::take(&mut self.pending);
        Some(self.prefixed(&rest))
    }

    fn prefixed(&self, line: &str) -> String {
        format!("{}> {line}", self.prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_split_across_chunks_carries_one_prefix() {
        let mut lines = PrefixedLines::new("hello");
        assert_eq!(lines.push("one\ntw"), ["hello> one"]);
        assert_eq!(lines.push("o\n"), ["hello> two"]);
        assert_eq!(lines.push("thr"), Vec::<String>::new());
        assert_eq!(lines.finish().as_deref(), Some("hello> thr"));
        assert_eq!(lines.finish(), None);
    }
}
