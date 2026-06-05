/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Minimal ANSI SGR (Select Graphic Rendition) state machine. Used while
//! chunking logs so each chunk can be rendered standalone: we record the SGR
//! sequence active at the chunk boundary and prepend it when serving the chunk.

#[derive(Clone, Default)]
pub struct SgrState {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    reverse: bool,
    strike: bool,
    fg: Option<Vec<u16>>,
    bg: Option<Vec<u16>>,
}

impl SgrState {
    /// Feed text, updating state for every complete `ESC [ ... m` sequence.
    pub fn apply_text(&mut self, text: &str) {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                let mut j = i + 2;
                while j < bytes.len() && !bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j >= bytes.len() {
                    break;
                }
                if bytes[j] == b'm' {
                    self.apply_params(&text[i + 2..j]);
                }
                i = j + 1;
                continue;
            }
            i += 1;
        }
    }

    fn apply_params(&mut self, params: &str) {
        let codes: Vec<u16> = if params.is_empty() {
            vec![0]
        } else {
            params.split(';').map(|p| p.parse().unwrap_or(0)).collect()
        };
        let mut k = 0;
        while k < codes.len() {
            match codes[k] {
                0 => *self = SgrState::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 => self.blink = true,
                7 => self.reverse = true,
                9 => self.strike = true,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.reverse = false,
                29 => self.strike = false,
                30..=37 | 39 | 90..=97 => self.fg = Some(vec![codes[k]]),
                40..=47 | 49 | 100..=107 => self.bg = Some(vec![codes[k]]),
                38 | 48 => {
                    let is_fg = codes[k] == 38;
                    let span = match codes.get(k + 1) {
                        Some(5) => 3,
                        Some(2) => 5,
                        _ => 1,
                    };
                    if span > 1 {
                        let end = (k + span).min(codes.len());
                        let value = codes[k..end].to_vec();
                        if is_fg {
                            self.fg = Some(value);
                        } else {
                            self.bg = Some(value);
                        }
                        k += span - 1;
                    }
                }
                _ => {}
            }
            k += 1;
        }
    }

    /// The minimal SGR sequence that reconstructs this state, or "" for default.
    pub fn to_prefix(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.bold {
            parts.push("1".into());
        }
        if self.dim {
            parts.push("2".into());
        }
        if self.italic {
            parts.push("3".into());
        }
        if self.underline {
            parts.push("4".into());
        }
        if self.blink {
            parts.push("5".into());
        }
        if self.reverse {
            parts.push("7".into());
        }
        if self.strike {
            parts.push("9".into());
        }
        if let Some(fg) = &self.fg {
            parts.push(
                fg.iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(";"),
            );
        }
        if let Some(bg) = &self.bg {
            parts.push(
                bg.iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(";"),
            );
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", parts.join(";"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SgrState;

    #[test]
    fn default_state_has_empty_prefix() {
        let s = SgrState::default();
        assert_eq!(s.to_prefix(), "");
    }

    #[test]
    fn carries_active_foreground_color() {
        let mut s = SgrState::default();
        s.apply_text("plain \x1b[31mred starts here");
        assert_eq!(s.to_prefix(), "\x1b[31m");
    }

    #[test]
    fn reset_clears_state() {
        let mut s = SgrState::default();
        s.apply_text("\x1b[1;31mbold red\x1b[0m back to normal");
        assert_eq!(s.to_prefix(), "");
    }

    #[test]
    fn combines_bold_and_color_minimally() {
        let mut s = SgrState::default();
        s.apply_text("\x1b[1m\x1b[34mbold blue");
        assert_eq!(s.to_prefix(), "\x1b[1;34m");
    }

    #[test]
    fn handles_256_color() {
        let mut s = SgrState::default();
        s.apply_text("\x1b[38;5;208morange");
        assert_eq!(s.to_prefix(), "\x1b[38;5;208m");
    }

    #[test]
    fn ignores_incomplete_escape_at_end() {
        let mut s = SgrState::default();
        s.apply_text("text \x1b[3");
        assert_eq!(s.to_prefix(), "");
    }
}
