/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use gradient_api_types::{ActionConfig, PatchGeneratorKind, PrGranularity, VerifyGate};
pub use gradient_entity::task_action::ActionType;

pub fn is_matrix_room_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('!') else {
        return false;
    };
    let (opaque, server) = rest
        .split_once(':')
        .map_or((rest, None), |(local, server)| (local, Some(server)));

    !opaque.is_empty()
        && server.is_none_or(|server| !server.is_empty())
        && !rest.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix(token: Option<&str>) -> ActionConfig {
        ActionConfig::SendMatrixMessage {
            homeserver: "https://matrix.example".into(),
            room_id: "!r:example".into(),
            access_token: token.map(Into::into),
        }
    }

    #[test]
    fn blank_secret_keeps_the_stored_one() {
        let mut edited = matrix(None);
        edited.keep_secret_from(matrix(Some("ENC")));
        assert_eq!(edited, matrix(Some("ENC")));
    }

    #[test]
    fn new_secret_wins_over_the_stored_one() {
        let mut edited = matrix(Some("NEW"));
        edited.keep_secret_from(matrix(Some("ENC")));
        assert_eq!(edited, matrix(Some("NEW")));
    }

    #[test]
    fn secret_field_matches_the_serialized_key() {
        for mut cfg in [
            ActionConfig::SendWebRequest {
                url: "https://x".into(),
                token: None,
            },
            matrix(None),
            ActionConfig::SendSlackMessage { webhook_url: None },
        ] {
            *cfg.secret_mut().unwrap() = Some("S".into());
            let json = serde_json::to_value(&cfg).unwrap();
            let field = cfg.action_type().secret_field().unwrap();
            assert_eq!(json[field], "S");
        }
    }

    #[test]
    fn room_id_needs_a_bang_and_an_id_with_an_optional_server() {
        assert!(is_matrix_room_id("!abc:example.org"));
        assert!(is_matrix_room_id(
            "!31hneApxJ_1o-63DmFrpeqnkFfWppnzWso1JvH3ogLM"
        ));
        assert!(!is_matrix_room_id("#ops:example.org"));
        assert!(!is_matrix_room_id("!"));
        assert!(!is_matrix_room_id("!abc:"));
        assert!(!is_matrix_room_id("!:example.org"));
        assert!(!is_matrix_room_id("!abc def"));
    }

    #[test]
    fn start_evaluation_round_trips_by_its_wire_name() {
        let task = "a0000000-0000-0000-0000-0000000000ff";
        let config: ActionConfig =
            serde_json::from_value(serde_json::json!({ "type": "start_evaluation", "task": task }))
                .unwrap();

        assert_eq!(
            config,
            ActionConfig::StartEvaluation {
                task: task.parse().unwrap()
            }
        );
        assert_eq!(config.action_type(), ActionType::StartEvaluation);
        assert_eq!(
            ActionType::from_name("start_evaluation"),
            Some(ActionType::StartEvaluation)
        );
    }
}
