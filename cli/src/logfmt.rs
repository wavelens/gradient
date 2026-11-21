/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build logs and server messages stay untrusted. They reach the terminal only through
//! `StreamingTerminalEscapeSequenceFilter`. Colour SGR sequences remain. All other escape
//! sequences, control characters and text-direction characters vanish. Carriage returns without
//! a line feed leave only the rest of the line visible, like a terminal.

use connector::api::MessageLevel;
use std::io::IsTerminal;

const MAX_SGR_PARAMETER_BYTE_LENGTH: usize = 64;
const MAX_UNFINISHED_LOG_LINE_BYTE_LENGTH: usize = 64 * 1024;

#[expect(
    clippy::disallowed_methods,
    reason = "check only whether stdout is a terminal"
)]
pub fn color_enabled() -> bool {
    std::io::stdout().is_terminal()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColorOrPlainText {
    KeepSgrColorSequences,
    StripEscapeSequences,
}

pub fn stdout_filter_mode() -> TerminalColorOrPlainText {
    if color_enabled() {
        TerminalColorOrPlainText::KeepSgrColorSequences
    } else {
        TerminalColorOrPlainText::StripEscapeSequences
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalEscapeSequenceParserState {
    PrintableText,
    EscapeByteRead,
    CsiParameterBytes(String),
    EscapeSequenceIntermediateBytes,
    InsideEscapeStringUntilStringTerminator { bel_also_terminates: bool },
    EscapeByteReadInString { bel_also_terminates: bool },
}

pub fn is_bidirectional_control_character(c: char) -> bool {
    matches!(
        c,
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

fn remove_concealed_text_parameter_from_sgr_parameters(parameters: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut colour_arguments_to_keep = 0;
    let mut previous = "";
    for parameter in parameters.split(';') {
        if colour_arguments_to_keep > 0 {
            colour_arguments_to_keep -= 1;
        } else if matches!(previous, "38" | "48" | "58") {
            colour_arguments_to_keep = match parameter {
                "5" => 1,
                "2" => 3,
                _ => 0,
            };
        } else if parameter == "8" {
            previous = parameter;
            continue;
        }
        previous = parameter;
        kept.push(parameter);
    }
    kept.join(";")
}

pub struct StreamingTerminalEscapeSequenceFilter {
    mode: TerminalColorOrPlainText,
    state: TerminalEscapeSequenceParserState,
    filtered_text_without_line_feed: String,
    carriage_return_clears_line_on_next_non_line_feed: bool,
}

impl StreamingTerminalEscapeSequenceFilter {
    pub fn new(mode: TerminalColorOrPlainText) -> Self {
        Self {
            mode,
            state: TerminalEscapeSequenceParserState::PrintableText,
            filtered_text_without_line_feed: String::new(),
            carriage_return_clears_line_on_next_non_line_feed: false,
        }
    }

    pub fn take_unfinished_line(&mut self) -> String {
        self.carriage_return_clears_line_on_next_non_line_feed = false;
        std::mem::take(&mut self.filtered_text_without_line_feed)
    }

    pub fn filter_text_chunk_holding_incomplete_escape(&mut self, chunk: &str) -> String {
        use TerminalEscapeSequenceParserState::*;
        let mut finished_lines = String::with_capacity(chunk.len());
        let mut filtered = std::mem::take(&mut self.filtered_text_without_line_feed);
        for next in chunk.chars() {
            let mut unread = Some(next);
            while let Some(c) = unread.take() {
                if self.state == PrintableText
                    && std::mem::take(&mut self.carriage_return_clears_line_on_next_non_line_feed)
                    && c != '\n'
                {
                    filtered.clear();
                }
                self.state = match std::mem::replace(&mut self.state, PrintableText) {
                    PrintableText => match c {
                        '\u{1b}' => EscapeByteRead,
                        '\u{9b}' => CsiParameterBytes(String::new()),
                        '\u{9d}' => InsideEscapeStringUntilStringTerminator {
                            bel_also_terminates: true,
                        },
                        '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                            InsideEscapeStringUntilStringTerminator {
                                bel_also_terminates: false,
                            }
                        }
                        '\n' => {
                            filtered.push(c);
                            finished_lines.push_str(&filtered);
                            filtered.clear();
                            PrintableText
                        }
                        '\r' => {
                            self.carriage_return_clears_line_on_next_non_line_feed = true;
                            PrintableText
                        }
                        '\t' => {
                            filtered.push(c);
                            PrintableText
                        }
                        c if c.is_control() || is_bidirectional_control_character(c) => {
                            PrintableText
                        }
                        c => {
                            filtered.push(c);
                            PrintableText
                        }
                    },
                    EscapeByteRead => match c {
                        '[' => CsiParameterBytes(String::new()),
                        ']' => InsideEscapeStringUntilStringTerminator {
                            bel_also_terminates: true,
                        },
                        'P' | 'X' | '^' | '_' => InsideEscapeStringUntilStringTerminator {
                            bel_also_terminates: false,
                        },
                        '\u{20}'..='\u{2f}' => EscapeSequenceIntermediateBytes,
                        '\u{30}'..='\u{7e}' => PrintableText,
                        _ => {
                            unread = Some(c);
                            PrintableText
                        }
                    },
                    CsiParameterBytes(mut parameters) => match c {
                        '\u{20}'..='\u{3f}' => {
                            if parameters.len() <= MAX_SGR_PARAMETER_BYTE_LENGTH {
                                parameters.push(c);
                            }
                            CsiParameterBytes(parameters)
                        }
                        'm' => {
                            if self.mode == TerminalColorOrPlainText::KeepSgrColorSequences
                                && parameters.len() <= MAX_SGR_PARAMETER_BYTE_LENGTH
                                && parameters
                                    .chars()
                                    .all(|p| p.is_ascii_digit() || p == ';' || p == ':')
                            {
                                let kept = remove_concealed_text_parameter_from_sgr_parameters(
                                    &parameters,
                                );
                                if parameters.is_empty() || !kept.is_empty() {
                                    filtered.push_str("\u{1b}[");
                                    filtered.push_str(&kept);
                                    filtered.push('m');
                                }
                            }
                            PrintableText
                        }
                        '\u{40}'..='\u{7e}' => PrintableText,
                        _ => {
                            unread = Some(c);
                            PrintableText
                        }
                    },
                    EscapeSequenceIntermediateBytes => match c {
                        '\u{20}'..='\u{2f}' => EscapeSequenceIntermediateBytes,
                        '\u{30}'..='\u{7e}' => PrintableText,
                        _ => {
                            unread = Some(c);
                            PrintableText
                        }
                    },
                    InsideEscapeStringUntilStringTerminator {
                        bel_also_terminates,
                    } => match c {
                        '\u{07}' if bel_also_terminates => PrintableText,
                        '\u{9c}' => PrintableText,
                        '\u{1b}' => EscapeByteReadInString {
                            bel_also_terminates,
                        },
                        _ => InsideEscapeStringUntilStringTerminator {
                            bel_also_terminates,
                        },
                    },
                    EscapeByteReadInString { .. } => match c {
                        '\\' => PrintableText,
                        _ => {
                            unread = Some(c);
                            EscapeByteRead
                        }
                    },
                };
            }
        }
        if filtered.len() > MAX_UNFINISHED_LOG_LINE_BYTE_LENGTH {
            finished_lines.push_str(&filtered);
        } else {
            self.filtered_text_without_line_feed = filtered;
        }
        finished_lines
    }
}

pub fn filter_complete_text_for_terminal_output(
    text: &str,
    mode: TerminalColorOrPlainText,
) -> String {
    let mut filter = StreamingTerminalEscapeSequenceFilter::new(mode);
    let mut filtered = filter.filter_text_chunk_holding_incomplete_escape(text);
    filtered.push_str(&filter.take_unfinished_line());
    filtered
}

pub fn eval_message_line(level: MessageLevel, message: &str) -> String {
    let (label, colour) = match level {
        MessageLevel::Error => ("error", "\u{1b}[1;31m"),
        MessageLevel::Warning => ("warning", "\u{1b}[1;33m"),
        MessageLevel::Notice => ("note", "\u{1b}[1;36m"),
    };
    let mode = stdout_filter_mode();
    let message = filter_complete_text_for_terminal_output(message, mode);
    match mode {
        TerminalColorOrPlainText::KeepSgrColorSequences => {
            format!("{colour}{label}:\u{1b}[0m {message}")
        }
        TerminalColorOrPlainText::StripEscapeSequences => format!("{label}: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLOUR: TerminalColorOrPlainText = TerminalColorOrPlainText::KeepSgrColorSequences;
    const PLAIN: TerminalColorOrPlainText = TerminalColorOrPlainText::StripEscapeSequences;

    fn filter(text: &str) -> String {
        filter_complete_text_for_terminal_output(text, COLOUR)
    }

    #[test]
    fn removes_osc52_clipboard_sequences_with_bel_and_st() {
        assert_eq!(filter("a\u{1b}]52;c;ZXZpbA==\u{7}b"), "ab");
        assert_eq!(filter("a\u{1b}]52;c;ZXZpbA==\u{1b}\\b"), "ab");
        assert_eq!(
            filter("a\u{1b}]8;;https://evil/\u{1b}\\link\u{1b}]8;;\u{1b}\\b"),
            "alinkb"
        );
        assert_eq!(filter("a\u{1b}Pq#0;payload\u{7}still\u{1b}\\b"), "ab");
    }

    #[test]
    fn removes_cursor_movement_and_line_erase_escape_sequences() {
        assert_eq!(filter("one\u{1b}[2A\u{1b}[2Ktwo"), "onetwo");
        assert_eq!(filter("x\u{1b}[?25ly\u{1b}(Bz\u{1b}7w"), "xyzw");
    }

    #[test]
    fn sgr_sequences_kept_in_colour_mode_and_removed_in_plain_mode() {
        let text = "a\u{1b}[1;31mred\u{1b}[0mb";
        assert_eq!(filter_complete_text_for_terminal_output(text, COLOUR), text);
        assert_eq!(
            filter_complete_text_for_terminal_output(text, PLAIN),
            "aredb"
        );
        assert_eq!(filter("\u{1b}[38:5:208mx"), "\u{1b}[38:5:208mx");
        assert_eq!(filter("\u{1b}[?1mx"), "x");
    }

    #[test]
    fn removes_eight_bit_c1_csi_and_osc_escape_sequences() {
        assert_eq!(filter("a\u{9b}2Kb"), "ab");
        assert_eq!(filter("a\u{9d}52;c;ZXZpbA==\u{9c}b"), "ab");
        assert_eq!(filter("a\u{9b}31mb"), "a\u{1b}[31mb");
    }

    #[test]
    fn bel_backspace_and_del_control_characters_are_removed() {
        assert_eq!(filter("a\u{7}b\u{8}c\u{7f}d\u{0}e\u{85}f"), "abcdef");
        assert_eq!(filter("tab\there\nnext"), "tab\there\nnext");
    }

    #[test]
    fn carriage_return_without_line_feed_keeps_only_text_after_it_on_that_line() {
        assert_eq!(filter("10%\r20%\n"), "20%\n");
        assert_eq!(filter("safe\rmalicious\nnext"), "malicious\nnext");
        assert_eq!(filter("a\r\nb"), "a\nb");
        assert_eq!(filter("done\r"), "done");
    }

    #[test]
    fn carriage_return_without_line_feed_keeps_later_text_across_input_chunks() {
        let mut filter = StreamingTerminalEscapeSequenceFilter::new(COLOUR);
        let mut shown = filter.filter_text_chunk_holding_incomplete_escape("10%\r");
        shown.push_str(&filter.filter_text_chunk_holding_incomplete_escape("20%"));
        shown.push_str(&filter.filter_text_chunk_holding_incomplete_escape("\r"));
        shown.push_str(&filter.filter_text_chunk_holding_incomplete_escape("\nnext"));
        shown.push_str(&filter.take_unfinished_line());
        assert_eq!(shown, "20%\nnext");
    }

    #[test]
    fn removes_unicode_text_direction_characters_from_filtered_log_text() {
        assert_eq!(
            filter("a\u{202a}b\u{202e}c\u{2066}d\u{2069}e\u{200e}f\u{200f}g\u{61c}h"),
            "abcdefgh"
        );
    }

    #[test]
    fn removes_ansi_concealed_text_parameter_but_keeps_color_index_8() {
        assert_eq!(filter("\u{1b}[8mhidden"), "hidden");
        assert_eq!(filter("\u{1b}[1;8;31mx"), "\u{1b}[1;31mx");
        assert_eq!(filter("\u{1b}[38;5;8mx"), "\u{1b}[38;5;8mx");
        assert_eq!(filter("\u{1b}[48;2;8;8;8;8mx"), "\u{1b}[48;2;8;8;8mx");
        assert_eq!(filter("\u{1b}[mx"), "\u{1b}[mx");
    }

    #[test]
    fn literal_backslash_u001b_stays_text_and_is_not_converted_to_escape() {
        assert_eq!(filter("\\u001b]52;c;eA=="), "\\u001b]52;c;eA==");
    }

    #[test]
    fn split_sgr_sequence_stays_whole_and_split_osc_sequence_leaks_nothing() {
        let mut filter = StreamingTerminalEscapeSequenceFilter::new(COLOUR);
        assert_eq!(
            filter.filter_text_chunk_holding_incomplete_escape("a\n\u{1b}[3"),
            "a\n"
        );
        assert_eq!(
            filter.filter_text_chunk_holding_incomplete_escape("1mred\n"),
            "\u{1b}[31mred\n"
        );

        let mut filter = StreamingTerminalEscapeSequenceFilter::new(COLOUR);
        assert_eq!(
            filter.filter_text_chunk_holding_incomplete_escape("a\n\u{1b}]52;c;ZXZ"),
            "a\n"
        );
        assert_eq!(
            filter.filter_text_chunk_holding_incomplete_escape("pbA==\u{1b}"),
            ""
        );
        assert_eq!(
            filter.filter_text_chunk_holding_incomplete_escape("\\b\n"),
            "b\n"
        );
    }

    #[test]
    fn test_coloured_evaluation_message_line_removes_escape_sequences_from_server_message() {
        for level in [
            MessageLevel::Error,
            MessageLevel::Warning,
            MessageLevel::Notice,
        ] {
            let line = eval_message_line(level, "boom\u{1b}]52;c;ZXZpbA==\u{7}\u{1b}[2K");
            assert!(line.contains("boom"));
            assert!(!line.contains("52;c"), "{line:?}");
            assert!(!line.contains("\u{1b}[2K"), "{line:?}");
        }
    }

    #[test]
    fn message_line_labels_by_level() {
        assert!(eval_message_line(MessageLevel::Error, "boom").contains("error:"));
        assert!(eval_message_line(MessageLevel::Warning, "careful").contains("warning:"));
        assert!(eval_message_line(MessageLevel::Notice, "fyi").contains("note:"));
    }
}
