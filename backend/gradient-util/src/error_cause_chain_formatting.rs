/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Join the error and its causes with `: `. Skip any cause whose text already appeared. Many wrappers, such as the IO errors of tungstenite, also print the source in their own message.
pub fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in std::iter::successors(Some(error), |e| e.source()) {
        let part = cause.to_string();
        if !parts.iter().any(|shown| shown.contains(&part)) {
            parts.push(part);
        }
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[derive(Debug)]
    struct Wrapped(String, Option<Box<Wrapped>>);

    impl fmt::Display for Wrapped {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|e| e as _)
        }
    }

    fn wrapped(text: &str, source: Option<Wrapped>) -> Wrapped {
        Wrapped(text.into(), source.map(Box::new))
    }

    #[test]
    fn joins_each_cause_under_its_context() {
        let error = wrapped(
            "deep GC unit nars/ql",
            Some(wrapped("listing failed", None)),
        );
        assert_eq!(error_chain(&error), "deep GC unit nars/ql: listing failed");
    }

    #[test]
    fn skips_a_cause_the_wrapper_already_printed() {
        let lookup = "failed to lookup address information: Name or service not known";
        let error = wrapped(
            "dial WebSocket wss://r3.invalid/proto",
            Some(wrapped(
                &format!("IO error: {lookup}"),
                Some(wrapped(lookup, None)),
            )),
        );
        assert_eq!(
            error_chain(&error),
            format!("dial WebSocket wss://r3.invalid/proto: IO error: {lookup}")
        );
    }
}
