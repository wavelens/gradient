/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_fixtures::rows::user;
use gradient_test_support::state::TestState;
use gradient_test_support::web::make_test_server;
use gradient_test_support::web::signed_in;
use gradient_types::{MUserSshKey, UserId, UserSshKeyId};
use serde_json::{Value, json};

const ED25519: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHfwJ61+Nu5yJhfB3PfAyywWMtpJcwybHAVPzGWnPQ6v test";
const ED25519_FINGERPRINT: &str = "SHA256:oriknbp5Jg0jqvSVYJ2XG6wWsyBelapZTV+AMQTOowo";
const ECDSA_P256: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBH7e0qqxxIFm8N39DMwZjKb/7Wz1kgx6zp5fpEQuKUIL/vNgwBT7aY+LQFlAZg1hQLcKZFV8/pwYiR4tvCvgPyM= test-p256";
const ECDSA_P256_FINGERPRINT: &str = "SHA256:jmbSbrUIr3Se1zoUXHWvSgjmb/yWgjcyKxObKwzD5aA";
const ECDSA_P384: &str = "ecdsa-sha2-nistp384 AAAAE2VjZHNhLXNoYTItbmlzdHAzODQAAAAIbmlzdHAzODQAAABhBFS0f4aHfIJfZ2AdKId98K4x+JvglmnOG7u2jNNdOUBj2nlwUDXdIm043nOxH0/mi13mDL7rbeLQNHGTePzTDYFLTHNfeXLxpicWCYQn3uJys8VMwKKEqQWIxE0J5f02ug== test-p384";
const ECDSA_P384_FINGERPRINT: &str = "SHA256:s7Xl8ASRQChJrTOBQnKJOD2xBpgo+CIwaEZeSDrzjB0";
const ECDSA_P521: &str = "ecdsa-sha2-nistp521 AAAAE2VjZHNhLXNoYTItbmlzdHA1MjEAAAAIbmlzdHA1MjEAAACFBABx7W7JoAEapctvZfVTuxlhfM8JdzpV6S3nLmmXDp3G8Eq+zWWxC6wVXTyTycixpkBWskBEbmZZliaAdnI+lL8GcQDSg9kVI5K1tARCYcvtTCHDJVkJTQ1vHV6+kx0irruA/AhxyW2bKTYcGCGemEvCkPpfmMj5kSEiYfKT63/UGF807w== test-p521";
const ECDSA_P521_FINGERPRINT: &str = "SHA256:qp4UnVqQ8ZrHSnassSPRPyuOMllzvBJ5mPbFTnfwfuI";
const RSA_2048: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC7VF7hHqPBCJyRg+jWqJXN6nHaNSKk59LCM+j72R9VihFN7vdwr5KcEeGsOsCnrHIz5rGNlOb+ensZ6oB7bIUoRdnFWKllZHPJ8eZ0WkFP7PvKvSbGEmdhdUOp9mrR3dqCmdc+1ghuzGmTXgeVPc3tBIB64EkRA48Fm8Soges4GEITAPvBoMOBQvRws3VqWCMbYDOpxJE1kV8JyywDQFeypXg7qVdN0uHWUQunqOM6yFqn7ob3Vw5J0BUaLX0Hm7q2jihpTzxN7upQ/M6W/FFjSocJPXk3GZgyR2XDSUzPKtGC1bevcclalLGPzctzf77Dzjq2wAENV5gHjB5YsI4t test-rsa2048";
const RSA_2048_FINGERPRINT: &str = "SHA256:dOlO2j/l/WNPjdDusxHrpgV13TVM/kyTKkY1Qmyj5WI";
const RSA_1024: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQDOEMBO6fopKht6WW+NdD8mxX6RZNA+0ngdNkOZ/DcA2iFY2D6ujXMfRtOUT//m62FAPcgfA+p/KqopPBCkBR8DA2v5E1lNE+W4arVknbwfUstxor8Xnhm/qG+gNLqIlWoRkXKFWjku0iM7ydsmELFpbxzz9+w96qIgHdgjPljRKQ== test-rsa1024";
const DSA: &str = "ssh-dss AAAAB3NzaC1kc3MAAACBAP test-dsa";

fn stored_key(owner: UserId) -> MUserSshKey {
    MUserSshKey {
        id: UserSshKeyId::now_v7(),
        user: owner,
        name: "laptop".into(),
        public_key: ED25519.into(),
        fingerprint: ED25519_FINGERPRINT.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn supported_keys_are_stored_and_unsafe_or_malformed_ones_are_refused() {
    let supported = [
        (ED25519, ED25519_FINGERPRINT),
        (ECDSA_P256, ECDSA_P256_FINGERPRINT),
        (ECDSA_P384, ECDSA_P384_FINGERPRINT),
        (ECDSA_P521, ECDSA_P521_FINGERPRINT),
        (RSA_2048, RSA_2048_FINGERPRINT),
    ];
    for (public_key, key_fingerprint) in supported {
        let (db, auth) = signed_in();
        let db = db
            .append_query_results([Vec::<MUserSshKey>::new()])
            .append_query_results([vec![MUserSshKey {
                public_key: public_key.into(),
                fingerprint: key_fingerprint.into(),
                ..stored_key(user().id)
            }]])
            .into_connection();
        let server = TestState::new()
            .web_db(db.clone())
            .cli(|cli| cli.ssh.enable = true)
            .server();

        let res = server
            .post("/api/v1/user/ssh-keys")
            .add_header("Authorization", auth.as_str())
            .json(&json!({ "name": "laptop", "public_key": public_key }))
            .await;

        res.assert_status_ok();
        assert_eq!(
            res.json::<Value>()["message"]["fingerprint"],
            key_fingerprint,
            "{public_key}"
        );
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains(key_fingerprint), "{log}");
    }

    let refused = [
        (RSA_1024, "RSA keys need at least 2048 bits."),
        (
            DSA,
            "DSA keys are not supported. Use an Ed25519 or ECDSA key.",
        ),
        ("ssh-ed25519 garbage", "Not an OpenSSH public key."),
        (
            "ssh-rsa AAAAB3NzaC1yc2E= truncated",
            "Not an OpenSSH public key.",
        ),
        ("", "Not an OpenSSH public key."),
    ];
    for (public_key, message) in refused {
        let (db, auth) = signed_in();
        let server = TestState::new()
            .web_db(db.into_connection())
            .cli(|cli| cli.ssh.enable = true)
            .server();

        let res = server
            .post("/api/v1/user/ssh-keys")
            .add_header("Authorization", auth.as_str())
            .json(&json!({ "name": "laptop", "public_key": public_key }))
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>()["message"], message, "{public_key}");
    }
}

#[tokio::test]
async fn a_known_fingerprint_is_a_conflict() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![stored_key(UserId::now_v7())]])
        .into_connection();
    let server = TestState::new()
        .web_db(db)
        .cli(|cli| cli.ssh.enable = true)
        .server();

    let res = server
        .post("/api/v1/user/ssh-keys")
        .add_header("Authorization", auth)
        .json(&json!({ "name": "laptop", "public_key": ED25519 }))
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn another_users_key_is_not_found() {
    let (db, auth) = signed_in();
    let foreign = stored_key(UserId::now_v7());
    let db = db
        .append_query_results([vec![foreign.clone()]])
        .into_connection();
    let server = TestState::new()
        .web_db(db.clone())
        .cli(|cli| cli.ssh.enable = true)
        .server();

    let res = server
        .delete(&format!("/api/v1/user/ssh-keys/{}", foreign.id))
        .add_header("Authorization", auth)
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
    let log = format!("{:?}", db.into_transaction_log());
    assert!(!log.contains("DELETE"), "{log}");
}

#[tokio::test]
async fn the_endpoints_are_missing_when_ssh_is_disabled() {
    let (db, auth) = signed_in();
    let db = db.into_connection();
    let server = make_test_server(db);

    let res = server
        .get("/api/v1/user/ssh-keys")
        .add_header("Authorization", auth)
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
}
