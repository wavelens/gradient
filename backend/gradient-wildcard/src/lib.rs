/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Gradient's only wildcard matcher is in this crate. Tenant patterns drive matching. One backtrack point is enough. Run time is proportional to the product of pattern length and candidate length. Exponential time cannot occur.

pub fn candidate_matches_wildcard_pattern(pattern: &str, candidate: &str) -> bool {
    match_pattern_with_optional_question_mark_single_character(pattern, candidate, true)
}

pub fn matches_pattern_with_only_star_wildcard(pattern: &str, candidate: &str) -> bool {
    match_pattern_with_optional_question_mark_single_character(pattern, candidate, false)
}

fn match_pattern_with_optional_question_mark_single_character(
    pattern: &str,
    candidate: &str,
    question_mark_matches_any_character: bool,
) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let candidate: Vec<char> = candidate.chars().collect();
    let (mut p, mut c) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;

    while c < candidate.len() {
        match pattern.get(p) {
            Some('*') => {
                backtrack = Some((p, c));
                p += 1;
            }
            Some('?') if question_mark_matches_any_character => {
                p += 1;
                c += 1;
            }
            Some(&ch) if ch == candidate[c] => {
                p += 1;
                c += 1;
            }
            _ => match backtrack {
                Some((star, matched)) => {
                    p = star + 1;
                    c = matched + 1;
                    backtrack = Some((star, matched + 1));
                }
                None => return false,
            },
        }
    }

    pattern[p..].iter().all(|&ch| ch == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn reference_recursive_wildcard_matcher(
        pattern: &[char],
        candidate: &[char],
        question_mark_matches_any_character: bool,
    ) -> bool {
        match pattern.split_first() {
            None => candidate.is_empty(),
            Some(('*', rest)) => {
                reference_recursive_wildcard_matcher(
                    rest,
                    candidate,
                    question_mark_matches_any_character,
                ) || (!candidate.is_empty()
                    && reference_recursive_wildcard_matcher(
                        pattern,
                        &candidate[1..],
                        question_mark_matches_any_character,
                    ))
            }
            Some(('?', rest)) if question_mark_matches_any_character => {
                !candidate.is_empty()
                    && reference_recursive_wildcard_matcher(
                        rest,
                        &candidate[1..],
                        question_mark_matches_any_character,
                    )
            }
            Some((ch, rest)) => {
                candidate.first() == Some(ch)
                    && reference_recursive_wildcard_matcher(
                        rest,
                        &candidate[1..],
                        question_mark_matches_any_character,
                    )
            }
        }
    }

    fn all_strings_up_to_length_over_alphabet(max_len: usize, alphabet: &[char]) -> Vec<String> {
        let mut all = vec![String::new()];
        let mut current = vec![String::new()];
        for _ in 0..max_len {
            current = current
                .iter()
                .flat_map(|prefix| alphabet.iter().map(move |ch| format!("{prefix}{ch}")))
                .collect();
            all.extend(current.iter().cloned());
        }
        all
    }

    #[test]
    fn wildcard_matcher_pathological_pattern_finishes_within_time_limit() {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let pattern = format!("{}b", "a*".repeat(40));
            let candidate = "a".repeat(10_000);
            let results = (
                candidate_matches_wildcard_pattern(&pattern, &candidate),
                matches_pattern_with_only_star_wildcard(&pattern, &candidate),
            );
            let _ = sender.send(results);
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)),
            Ok((false, false))
        );
    }

    #[test]
    fn wildcard_match_functions_agree_with_recursive_reference_for_all_patterns_and_candidates_up_to_length_five_in_both_question_mark_modes()
     {
        let strings = all_strings_up_to_length_over_alphabet(5, &['a', 'b', '*', '?']);
        for pattern in &strings {
            let pattern_chars: Vec<char> = pattern.chars().collect();
            for candidate in &strings {
                let candidate_chars: Vec<char> = candidate.chars().collect();
                assert_eq!(
                    candidate_matches_wildcard_pattern(pattern, candidate),
                    reference_recursive_wildcard_matcher(&pattern_chars, &candidate_chars, true),
                    "{pattern:?} {candidate:?}"
                );
                assert_eq!(
                    matches_pattern_with_only_star_wildcard(pattern, candidate),
                    reference_recursive_wildcard_matcher(&pattern_chars, &candidate_chars, false),
                    "{pattern:?} {candidate:?}"
                );
            }
        }
    }
}
