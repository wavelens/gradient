/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use crate::resource::Resource;
use gradient_types::{StoreHash, StorePathMapper};

pub(crate) fn stored_hash(mapper: &StorePathMapper, hash: &str) -> WebResult<String> {
    let hash = StoreHash::parse(hash).map_err(|_| WebError::not_found(Resource::Path))?;
    Ok(mapper.inbound_hash(hash).to_string())
}

pub(crate) fn original_text(mapper: &StorePathMapper, text: String) -> String {
    mapper.outbound_text(&text).unwrap_or(text)
}
