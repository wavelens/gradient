/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_test_support::state::TestState;
use gradient_web::create_router;

#[tokio::test]
async fn building_router_starts_no_supervised_background_service() {
    let state = TestState::new().build();
    let _router = create_router(Arc::clone(&state)).expect("router");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(state.shutdown.tree().is_none());
}
