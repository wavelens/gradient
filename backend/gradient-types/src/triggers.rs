/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Six fields, `sec min hour dom mon dow`, are the expected input for the `cron` crate (v0.16).
//! 5-field crontab expressions receive a leading `0` seconds field before saving.

use crate::ids::TaskTriggerId;
use chrono::NaiveDateTime;

pub use gradient_api_types::{
    ChangedFilePathFilter, MAX_TRIGGER_PATH_FILTER_PATTERN_CHARACTER_COUNT,
    MAX_TRIGGER_PATH_FILTER_PATTERN_COUNT, TriggerConfig, TriggerConfigError,
};
pub use gradient_entity::task::ConcurrencyPolicy;
pub use gradient_entity::task_trigger::TriggerType;

const POLLING_JITTER_PCT: u32 = 10;

pub fn next_poll_time_with_jitter(
    trigger_id: TaskTriggerId,
    last_fired_at: NaiveDateTime,
    interval_secs: u32,
) -> NaiveDateTime {
    let jitter = polling_jitter_secs(trigger_id, last_fired_at, interval_secs);
    last_fired_at + chrono::Duration::seconds(i64::from(interval_secs) + i64::from(jitter))
}

fn polling_jitter_secs(
    trigger_id: TaskTriggerId,
    last_fired_at: NaiveDateTime,
    interval_secs: u32,
) -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let max = interval_secs / (100 / POLLING_JITTER_PCT);
    if max == 0 {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    trigger_id.hash(&mut hasher);
    last_fired_at.hash(&mut hasher);
    u32::try_from(hasher.finish() % (u64::from(max) + 1)).unwrap_or(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            failure_persistence: None,
            ..proptest::prelude::ProptestConfig::default()
        })]

        #[test]
        fn next_poll_time_with_jitter_is_deterministic_and_within_bounds(
            id in proptest::prelude::any::<u128>(),
            last_secs in 0_i64..=4_102_444_800,
            interval in proptest::prelude::any::<u32>(),
        ) {
            let id = TaskTriggerId::new(uuid::Uuid::from_u128(id));
            let last = chrono::DateTime::from_timestamp(last_secs, 0).unwrap().naive_utc();
            let next = next_poll_time_with_jitter(id, last, interval);
            let waited = (next - last).num_seconds();
            proptest::prop_assert_eq!(next, next_poll_time_with_jitter(id, last, interval));
            proptest::prop_assert!(waited >= i64::from(interval));
            proptest::prop_assert!(waited <= i64::from(interval) + i64::from(interval / 10));
        }
    }

    #[test]
    fn polling_jitter_changes_between_cycles() {
        let id = TaskTriggerId::now_v7();
        let a = polling_jitter_secs(id, dt("2026-05-06 10:00:00"), 600);
        let b = polling_jitter_secs(id, dt("2026-05-06 10:11:00"), 600);
        let c = polling_jitter_secs(id, dt("2026-05-06 10:22:00"), 600);
        assert!(a != b || a != c, "jitter must vary across cycles");
    }

    #[test]
    fn polling_jitter_zero_when_interval_below_threshold() {
        let last = dt("2026-05-06 10:00:00");
        assert_eq!(
            next_poll_time_with_jitter(TaskTriggerId::now_v7(), last, 9),
            dt("2026-05-06 10:00:09")
        );
    }

    #[test]
    fn polling_config_round_trip() {
        let cfg = TriggerConfig::Polling {
            interval_secs: 60,
            branch: None,
            path_filter: ChangedFilePathFilter::default(),
        };
        let db = cfg.to_db_json();
        assert!(
            db.get("type").is_none(),
            "db json should not carry the type tag"
        );
        let parsed = TriggerConfig::parse_row(TriggerType::Polling, &db).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn polling_config_with_branch_round_trip() {
        let cfg = TriggerConfig::Polling {
            interval_secs: 120,
            branch: Some("develop".into()),
            path_filter: ChangedFilePathFilter::default(),
        };
        let db = cfg.to_db_json();
        assert_eq!(db["branch"], serde_json::json!("develop"));
        let parsed = TriggerConfig::parse_row(TriggerType::Polling, &db).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn trigger_config_parse_row_round_trip_for_post_request_with_and_without_branch() {
        let without_branch =
            TriggerConfig::parse_row(TriggerType::PostRequest, &serde_json::json!({})).unwrap();
        assert_eq!(
            without_branch,
            TriggerConfig::PostRequest {
                branch: None,
                path_filter: ChangedFilePathFilter::default(),
            }
        );
        let with_branch = TriggerConfig::parse_row(
            TriggerType::PostRequest,
            &serde_json::json!({ "branch": "main", "include_paths": ["src/**"] }),
        )
        .unwrap();
        let src_only = ChangedFilePathFilter {
            include_paths: vec!["src/**".into()],
            exclude_paths: vec![],
        };
        assert_eq!(
            with_branch,
            TriggerConfig::PostRequest {
                branch: Some("main".into()),
                path_filter: src_only.clone(),
            }
        );
        for cfg in [without_branch, with_branch.clone()] {
            let parsed =
                TriggerConfig::parse_row(TriggerType::PostRequest, &cfg.to_db_json()).unwrap();
            assert_eq!(parsed, cfg);
        }
        assert_eq!(with_branch.trigger_branch(), Some("main"));
        assert_eq!(with_branch.path_filter(), Some(&src_only));
        assert_eq!(
            with_branch.with_branch("release".into()).path_filter(),
            Some(&src_only)
        );
    }

    #[test]
    fn trigger_branch_skips_glob_patterns() {
        let cfg = TriggerConfig::ReporterPush {
            branches: vec!["release/*".into(), "feat-[ab]".into(), "develop".into()],
            tags: vec![],
            releases_only: false,
            include_flake_inputs: false,
            path_filter: ChangedFilePathFilter::default(),
        };
        assert_eq!(cfg.trigger_branch(), Some("develop"));
    }

    #[test]
    fn trigger_branch_without_a_literal_branch_is_the_default_branch() {
        let cfg = TriggerConfig::ReporterPullRequest {
            branches: vec!["*".into()],
            actions: vec![],
            require_approval: true,
            path_filter: ChangedFilePathFilter::default(),
        };
        assert_eq!(cfg.trigger_branch(), None);
    }

    #[test]
    fn polling_under_10_seconds_rejected() {
        let cfg = TriggerConfig::Polling {
            interval_secs: 5,
            branch: None,
            path_filter: ChangedFilePathFilter::default(),
        };
        assert!(matches!(
            cfg.validate(),
            Err(TriggerConfigError::PollingIntervalTooSmall)
        ));
    }

    #[test]
    fn cron_invalid_rejected() {
        let cfg = TriggerConfig::Time {
            cron: "not a cron".into(),
            branch: None,
        };
        assert!(matches!(
            cfg.validate(),
            Err(TriggerConfigError::InvalidCron(_))
        ));
    }

    #[test]
    fn cron_valid_accepted() {
        let cfg = TriggerConfig::Time {
            cron: "0 0 2 * * *".into(),
            branch: None,
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn five_field_cron_is_stored_in_the_six_field_form() {
        let cfg = TriggerConfig::Time {
            cron: " 0 2 * * * ".into(),
            branch: Some("release".into()),
        };
        assert_eq!(
            cfg.validate_and_convert_cron_to_six_fields().unwrap(),
            TriggerConfig::Time {
                cron: "0 0 2 * * *".into(),
                branch: Some("release".into()),
            }
        );
    }

    #[test]
    fn six_field_cron_is_kept() {
        let cfg = TriggerConfig::Time {
            cron: "0 30 4 * * Mon".into(),
            branch: None,
        };
        assert_eq!(
            cfg.clone()
                .validate_and_convert_cron_to_six_fields()
                .unwrap(),
            cfg
        );
    }

    #[test]
    fn time_trigger_branch_is_the_trigger_branch() {
        let cfg = TriggerConfig::Time {
            cron: "0 0 2 * * *".into(),
            branch: Some("release".into()),
        };
        assert_eq!(cfg.trigger_branch(), Some("release"));
    }

    #[test]
    fn time_rows_without_branch_read_as_the_default_branch() {
        let legacy_db = serde_json::json!({ "cron": "0 0 2 * * *" });
        let parsed = TriggerConfig::parse_row(TriggerType::Time, &legacy_db).unwrap();
        assert_eq!(parsed.trigger_branch(), None);
    }

    #[test]
    fn type_mismatch_rejected() {
        let bad = serde_json::json!({"interval_secs": 60});
        let res = TriggerConfig::parse_row(TriggerType::Time, &bad);
        assert!(res.is_err(), "expected error, got {res:?}");
    }

    #[test]
    fn reporter_pull_request_require_approval_defaults_true_for_legacy_rows() {
        // Pre-#247 rows are lacking `require_approval` in the stored JSON. The serde default must
        // read as `true` to keep them secure by default without a backfill migration.
        let legacy_db = serde_json::json!({
            "integration_id": "00000000-0000-0000-0000-000000000000",
            "branches": [],
            "actions": ["opened"],
        });
        let parsed =
            TriggerConfig::parse_row(TriggerType::ReporterPullRequest, &legacy_db).unwrap();
        let TriggerConfig::ReporterPullRequest {
            require_approval, ..
        } = parsed
        else {
            panic!("expected ReporterPullRequest");
        };
        assert!(require_approval, "missing field must default to true");
    }

    #[test]
    fn rows_stored_before_path_filters_read_as_unfiltered() {
        let legacy_push = serde_json::json!({
            "branches": ["main"],
            "tags": [],
            "releases_only": false,
        });
        let legacy_polling = serde_json::json!({ "interval_secs": 60 });
        for (trigger_type, row) in [
            (TriggerType::ReporterPush, legacy_push),
            (TriggerType::Polling, legacy_polling),
        ] {
            let parsed = TriggerConfig::parse_row(trigger_type, &row).unwrap();
            assert_eq!(
                parsed.path_filter(),
                Some(&ChangedFilePathFilter::default())
            );
        }
    }

    #[test]
    fn path_filters_round_trip_through_the_stored_row() {
        let cfg = TriggerConfig::ReporterPullRequest {
            branches: vec![],
            actions: vec!["opened".into()],
            require_approval: false,
            path_filter: ChangedFilePathFilter {
                include_paths: vec!["packages/kernel/**".into()],
                exclude_paths: vec!["*.md".into()],
            },
        };
        let db = cfg.to_db_json();
        assert_eq!(
            db["include_paths"],
            serde_json::json!(["packages/kernel/**"])
        );
        assert_eq!(db["exclude_paths"], serde_json::json!(["*.md"]));
        let parsed = TriggerConfig::parse_row(TriggerType::ReporterPullRequest, &db).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn absolute_and_empty_path_patterns_rejected() {
        let absolute = serde_json::json!({ "interval_secs": 60, "include_paths": ["/abs"] });
        assert!(matches!(
            TriggerConfig::parse_row(TriggerType::Polling, &absolute),
            Err(TriggerConfigError::PathFilterNotRelative(p)) if p == "/abs"
        ));
        let empty = serde_json::json!({ "branches": [], "exclude_paths": [""] });
        assert!(matches!(
            TriggerConfig::parse_row(TriggerType::ReporterPush, &empty),
            Err(TriggerConfigError::EmptyPathFilterPattern)
        ));
    }

    #[test]
    fn create_rejects_too_many_or_too_long_patterns_but_parse_row_accepts_them() {
        let polling_with_patterns =
            |include_paths: Vec<String>, exclude_paths: Vec<String>| TriggerConfig::Polling {
                interval_secs: 60,
                branch: None,
                path_filter: ChangedFilePathFilter {
                    include_paths,
                    exclude_paths,
                },
            };
        let patterns = |count: usize| (0..count).map(|i| format!("src/{i}/*")).collect();
        let at_limits = polling_with_patterns(
            patterns(MAX_TRIGGER_PATH_FILTER_PATTERN_COUNT - 1),
            vec!["é".repeat(MAX_TRIGGER_PATH_FILTER_PATTERN_CHARACTER_COUNT)],
        );
        assert!(at_limits.validate_path_filter_pattern_limits().is_ok());

        let too_many = polling_with_patterns(
            patterns(MAX_TRIGGER_PATH_FILTER_PATTERN_COUNT),
            vec!["*.md".into()],
        );
        assert!(matches!(
            too_many.validate_path_filter_pattern_limits(),
            Err(TriggerConfigError::PathFilterHasTooManyPatterns { max_count: 64 })
        ));
        let too_long = polling_with_patterns(
            vec!["a".repeat(MAX_TRIGGER_PATH_FILTER_PATTERN_CHARACTER_COUNT + 1)],
            vec![],
        );
        assert!(matches!(
            too_long.validate_path_filter_pattern_limits(),
            Err(TriggerConfigError::PathFilterPatternTooLong { max_length: 512 })
        ));

        for stored in [too_many, too_long] {
            let parsed = TriggerConfig::parse_row(TriggerType::Polling, &stored.to_db_json());
            assert_eq!(parsed.unwrap(), stored);
        }
    }

    #[test]
    fn with_branch_keeps_the_path_filter() {
        let path_filter = ChangedFilePathFilter {
            include_paths: vec!["src/*".into()],
            exclude_paths: vec!["src/vendor/*".into()],
        };
        let push = TriggerConfig::ReporterPush {
            branches: vec!["main".into()],
            tags: vec![],
            releases_only: false,
            include_flake_inputs: false,
            path_filter: path_filter.clone(),
        };
        assert_eq!(
            push.with_branch("release".into()).path_filter(),
            Some(&path_filter)
        );
    }

    #[test]
    fn with_branch_replaces_the_branch_of_every_kind() {
        let release = || "release".to_owned();
        let polling = TriggerConfig::Polling {
            interval_secs: 60,
            branch: None,
            path_filter: ChangedFilePathFilter::default(),
        };
        let time = TriggerConfig::Time {
            cron: "0 0 3 * * *".into(),
            branch: Some("main".into()),
        };
        let push = TriggerConfig::ReporterPush {
            branches: vec!["main".into(), "dev".into()],
            tags: vec!["v*".into()],
            releases_only: false,
            include_flake_inputs: true,
            path_filter: ChangedFilePathFilter::default(),
        };
        for cfg in [polling, time, push] {
            let moved = cfg.with_branch(release());
            assert_eq!(moved.trigger_branch(), Some("release"));
            assert_eq!(
                moved.branch_names_or_patterns_for_task_header(),
                vec![Some(release())]
            );
        }
    }

    #[test]
    fn an_empty_branch_filter_reads_as_any_branch_in_the_header() {
        let push = TriggerConfig::ReporterPullRequest {
            branches: vec![],
            actions: vec![],
            require_approval: true,
            path_filter: ChangedFilePathFilter::default(),
        };
        assert_eq!(
            push.branch_names_or_patterns_for_task_header(),
            vec![Some("*".to_owned())]
        );
        let polling = TriggerConfig::Polling {
            interval_secs: 60,
            branch: None,
            path_filter: ChangedFilePathFilter::default(),
        };
        assert_eq!(
            polling.branch_names_or_patterns_for_task_header(),
            vec![None]
        );
    }
}
