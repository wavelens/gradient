/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{ProjectId, UserId, WorkerRegistrationId};
use gradient_entity::worker_registration;

pub fn worker_registration(active: bool, enable_eval: bool) -> worker_registration::Model {
    worker_registration::Model {
        id: WorkerRegistrationId::now_v7(),
        peer_id: ProjectId::nil(),
        worker_id: "00000000-0000-4000-8000-000000000001".into(),
        active,
        enable_fetch: true,
        enable_eval,
        enable_build: true,
        created_by: Some(UserId::nil()),
        ..Default::default()
    }
}
