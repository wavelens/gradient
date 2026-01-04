/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod common;

use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use tower_http::cors::{AllowOrigin, CorsLayer};

#[test]
fn test_middleware_configuration() {
    let state = common::create_mock_state();

    // Test CORS configuration creation doesn't panic
    let cors_allow_origin = AllowOrigin::exact(state.cli.serve_url.clone().try_into().unwrap());

    // Test that CORS configuration is properly created
    let _cors = CorsLayer::new()
        .allow_origin(cors_allow_origin)
        .allow_headers(vec![AUTHORIZATION, ACCEPT, CONTENT_TYPE])
        .allow_credentials(true);
}
