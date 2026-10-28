/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_redact::SecretRedactor;
use gradient_wire::guest::{GuestSecret, RunJob};

#[derive(Clone)]
pub struct JobSecrets {
    pub secrets: Vec<GuestSecret>,
    pub token: String,
}

impl JobSecrets {
    pub fn of(job: &RunJob) -> Self {
        Self {
            secrets: job.secrets.clone(),
            token: job.context.job.get("token").cloned().unwrap_or_default(),
        }
    }

    pub fn redactor(&self) -> SecretRedactor {
        let token = (!self.token.is_empty()).then_some(("job-token", self.token.as_bytes()));
        SecretRedactor::new(
            self.secrets
                .iter()
                .map(|secret| (secret.name.as_str(), secret.value.as_slice()))
                .chain(token),
        )
    }
}

pub fn job_redactor(job: &RunJob) -> SecretRedactor {
    JobSecrets::of(job).redactor()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gradient_wire::guest::ContextLayers;

    use super::*;

    #[test]
    fn the_job_token_and_every_secret_are_masked() {
        let job = RunJob {
            secrets: vec![GuestSecret {
                name: "wandb-token".into(),
                value: b"s3cr3t-value".to_vec(),
            }],
            context: ContextLayers {
                job: BTreeMap::from([("token".into(), "gr_job_abcdefgh123".into())]),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            job_redactor(&job).redact(b"a gr_job_abcdefgh123 b s3cr3t-value"),
            b"a [secret:job-token] b [secret:wandb-token]"
        );
    }
}
