/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use nix_bindings::eval_cache::AttrCursor;
use nix_bindings::{EvalState, Value, ValueType};

use crate::ipc::License;

pub fn licenses_of(cursor: &AttrCursor, state: &EvalState) -> Vec<License> {
    read_licenses(cursor, state).unwrap_or_else(|e| {
        tracing::debug!(error = %e, "unreadable meta.license");
        Vec::new()
    })
}

pub fn from_fields(spdx_id: Option<String>, short_name: Option<String>) -> Option<License> {
    match (spdx_id, short_name) {
        (Some(id), _) => Some(License::Spdx { id }),
        (None, Some(name)) => Some(License::Name { name }),
        (None, None) => None,
    }
}

fn read_licenses(cursor: &AttrCursor, state: &EvalState) -> Result<Vec<License>> {
    let Some(license) = cursor
        .maybe_get_attr("meta")?
        .map(|meta| meta.maybe_get_attr("license"))
        .transpose()?
        .flatten()
    else {
        return Ok(Vec::new());
    };

    if let Ok(name) = license.string() {
        return Ok(vec![License::Name { name }]);
    }

    if let Ok(Some(found)) = cached_fields(&license) {
        return Ok(vec![found]);
    }

    let value = license.value(state)?;
    Ok(value
        .list_iter()?
        .filter_map(|element| element.ok().and_then(|v| from_value(&v)))
        .collect())
}

fn cached_fields(license: &AttrCursor) -> Result<Option<License>> {
    let field = |name: &str| -> Result<Option<String>> {
        license
            .maybe_get_attr(name)?
            .map(|c| c.string())
            .transpose()
            .map_err(Into::into)
    };
    Ok(from_fields(field("spdxId")?, field("shortName")?))
}

fn from_value(value: &Value<'_>) -> Option<License> {
    match value.value_type() {
        ValueType::String => value.as_string().ok().map(|name| License::Name { name }),
        ValueType::Attrs => {
            let field = |name: &str| {
                value
                    .has_attr(name)
                    .ok()
                    .filter(|present| *present)
                    .and_then(|_| value.get_attr(name).ok())
                    .and_then(|v| v.as_string().ok())
            };
            from_fields(field("spdxId"), field("shortName"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spdx_id_wins_over_short_name() {
        assert_eq!(
            from_fields(Some("MIT".into()), Some("mit".into())),
            Some(License::Spdx { id: "MIT".into() })
        );
    }

    #[test]
    fn short_name_without_spdx_id_is_a_name() {
        assert_eq!(
            from_fields(None, Some("unfree".into())),
            Some(License::Name {
                name: "unfree".into()
            })
        );
    }

    #[test]
    fn no_fields_is_no_license() {
        assert_eq!(from_fields(None, None), None);
    }
}
