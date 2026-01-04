/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct CommitResponse {
    pub id: String,
    pub message: String,
    pub hash: String,
}

pub async fn get_commit(
    config: RequestConfig,
    commit_id: String,
) -> Result<BaseResponse<CommitResponse>, String> {
    let res = get_client(
        config,
        format!("commits/{}", commit_id),
        RequestType::GET,
        true,
    )
    .unwrap()
    .send()
    .await
    .unwrap();

    Ok(parse_response(res).await)
}
