/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::messages::FailedPeer;

/// Verifies `token` against a stored `token_hash` in constant time.
///
/// Accepts two storage formats:
/// - PHC strings (e.g. `$argon2id$...`) - verified via `password_auth`,
///   which is constant-time and salted/KDF-hardened. This is what new
///   registrations write.
/// - Lowercase hex SHA-256 - legacy format from older registrations,
///   compared in constant time via `subtle::ConstantTimeEq`. New rows
///   are never written in this format.
pub fn verify_token(token: &str, token_hash: &str) -> bool {
    if token_hash.starts_with('$') {
        password_auth::verify_password(token, token_hash).is_ok()
    } else {
        use sha2::{Digest, Sha256};
        use subtle::ConstantTimeEq;
        let digest = hex::encode(Sha256::digest(token.as_bytes()));
        digest.as_bytes().ct_eq(token_hash.as_bytes()).into()
    }
}

/// Validates `auth_tokens` (worker-supplied `(peer_id, plaintext_token)`) against
/// `registered_peers` (`(peer_id, stored_token_hash)`).
///
/// Returns `(authorized_peers, failed_peers)`.
pub fn validate_tokens(
    registered_peers: &[(String, String)],
    auth_tokens: &[(String, String)],
) -> (Vec<String>, Vec<FailedPeer>) {
    let mut authorized = Vec::new();
    let mut failed = Vec::new();

    for (peer_id, token_hash) in registered_peers {
        match auth_tokens.iter().find(|(pid, _)| pid == peer_id) {
            Some((_, token)) => {
                if verify_token(token, token_hash) {
                    authorized.push(peer_id.clone());
                } else {
                    failed.push(FailedPeer {
                        peer_id: peer_id.clone(),
                        reason: "invalid token".into(),
                    });
                }
            }
            None => {
                failed.push(FailedPeer {
                    peer_id: peer_id.clone(),
                    reason: "no token provided".into(),
                });
            }
        }
    }

    (authorized, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256_hex(s: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(s.as_bytes()))
    }

    fn argon2(s: &str) -> String {
        password_auth::generate_hash(s)
    }

    #[test]
    fn validate_tokens_matching_hash_authorizes() {
        let token = "my-secret-token";
        let hash = sha256_hex(token);
        let registered = vec![("peer-a".to_string(), hash)];
        let auth = vec![("peer-a".to_string(), token.to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_wrong_hash_fails() {
        let registered = vec![("peer-a".to_string(), sha256_hex("correct-token"))];
        let auth = vec![("peer-a".to_string(), "wrong-token".to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert!(authorized.is_empty());
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].peer_id, "peer-a");
        assert!(failed[0].reason.contains("invalid token"));
    }

    #[test]
    fn validate_tokens_missing_token_fails() {
        let registered = vec![("peer-a".to_string(), sha256_hex("some-token"))];
        let (authorized, failed) = validate_tokens(&registered, &[]);
        assert!(authorized.is_empty());
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].peer_id, "peer-a");
        assert!(failed[0].reason.contains("no token provided"));
    }

    #[test]
    fn validate_tokens_mixed_results() {
        let tok_b = "token-b";
        let registered = vec![
            ("peer-a".to_string(), sha256_hex("token-a")),
            ("peer-b".to_string(), sha256_hex(tok_b)),
            ("peer-c".to_string(), sha256_hex("token-c")),
        ];
        let auth = vec![
            ("peer-a".to_string(), "token-a".to_string()), // correct
            ("peer-b".to_string(), "wrong".to_string()),   // wrong hash
                                                           // peer-c missing
        ];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert_eq!(failed.len(), 2);
        let failed_ids: Vec<&str> = failed.iter().map(|f| f.peer_id.as_str()).collect();
        assert!(failed_ids.contains(&"peer-b"));
        assert!(failed_ids.contains(&"peer-c"));
    }

    #[test]
    fn validate_tokens_empty_inputs() {
        let (authorized, failed) = validate_tokens(&[], &[]);
        assert!(authorized.is_empty());
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_extra_tokens_ignored() {
        let auth = vec![("unknown-peer".to_string(), "some-token".to_string())];
        let (authorized, failed) = validate_tokens(&[], &auth);
        assert!(authorized.is_empty());
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_argon2_hash_authorizes() {
        let token = "argon2-token";
        let registered = vec![("peer-a".to_string(), argon2(token))];
        let auth = vec![("peer-a".to_string(), token.to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_argon2_wrong_token_fails() {
        let registered = vec![("peer-a".to_string(), argon2("correct"))];
        let auth = vec![("peer-a".to_string(), "wrong".to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert!(authorized.is_empty());
        assert_eq!(failed.len(), 1);
        assert!(failed[0].reason.contains("invalid token"));
    }

    #[test]
    fn verify_token_dispatches_on_format() {
        let token = "tok";
        // PHC string → argon2 path
        let phc = argon2(token);
        assert!(phc.starts_with('$'), "argon2 hash must start with $");
        assert!(verify_token(token, &phc));
        // Hex SHA-256 → legacy path
        assert!(verify_token(token, &sha256_hex(token)));
        // Wrong inputs both reject
        assert!(!verify_token("bad", &phc));
        assert!(!verify_token("bad", &sha256_hex(token)));
    }

    #[test]
    fn validate_tokens_duplicate_peer_first_wins() {
        let token = "correct";
        let registered = vec![("peer-a".to_string(), sha256_hex(token))];
        let auth = vec![
            ("peer-a".to_string(), token.to_string()),
            ("peer-a".to_string(), "wrong".to_string()),
        ];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert!(failed.is_empty());
    }
}
