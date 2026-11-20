/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::{HeaderMap, header};

pub(crate) fn request_accepts_media_type(headers: &HeaderMap, media_type: &str) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|media_range| {
            let mut parts = media_range.split(';');
            let matches_media_type = parts
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case(media_type);
            matches_media_type
                && !parts.any(|parameter| {
                    parameter.split_once('=').is_some_and(|(name, value)| {
                        name.trim().eq_ignore_ascii_case("q")
                            && value.trim().parse::<f32>().is_ok_and(|q| q == 0.0)
                    })
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn header_map_with_accept_lines(lines: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for line in lines {
            headers.append(header::ACCEPT, HeaderValue::from_str(line).unwrap());
        }
        headers
    }

    #[test]
    fn accept_header_media_range_q_zero_is_not_accepted_and_positive_q_is_accepted() {
        for accept in ["text/html;q=0", "text/html; q=0.0", "text/html;Q=0.000"] {
            assert!(
                !request_accepts_media_type(&header_map_with_accept_lines(&[accept]), "text/html"),
                "{accept}"
            );
        }
        for accept in ["text/html;q=0.001", "text/html;q=1", "text/html;level=1"] {
            assert!(
                request_accepts_media_type(&header_map_with_accept_lines(&[accept]), "text/html"),
                "{accept}"
            );
        }
    }

    #[test]
    fn media_type_matching_ignores_case_and_whitespace_and_reads_every_accept_header_line() {
        let headers = header_map_with_accept_lines(&["application/json", " Text/Plain ;q=0.5"]);
        assert!(request_accepts_media_type(&headers, "text/plain"));
        assert!(!request_accepts_media_type(&headers, "text/html"));
        assert!(!request_accepts_media_type(&HeaderMap::new(), "text/plain"));
    }
}
