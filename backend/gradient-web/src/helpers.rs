/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared response / lookup helpers used across `endpoints/`.

use crate::error::{WebError, WebResult};
use axum::Json;
use gradient_types::ids::RoleId;
use gradient_types::{BaseResponse, CRole, ERole, Paginated, PaginationParams};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, SelectorTrait,
};
use std::collections::HashMap;

/// Wraps a value in the standard successful `BaseResponse` envelope.
/// Replaces the boilerplate `Json(BaseResponse { error: false, message })`.
#[inline]
pub fn ok_json<T>(message: T) -> Json<BaseResponse<T>> {
    Json(BaseResponse {
        error: false,
        message,
    })
}

/// Convert an `Option<T>` (typically the result of a SeaORM `.one(db).await?`
/// lookup) into a `WebResult<T>`, mapping `None` to `WebError::NotFound`
/// with a `"<resource> not found"` message.
pub trait OptionExt<T> {
    fn or_not_found(self, resource: &str) -> WebResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn or_not_found(self, resource: &str) -> WebResult<T> {
        self.ok_or_else(|| WebError::not_found(resource))
    }
}

/// Run a SeaORM query as one page of a paginated listing, collapsing the
/// `page()/per_page()/num_items()/fetch_page()` idiom repeated across list
/// handlers. Callers that post-process rows map over the returned
/// [`Paginated::map`] result.
pub async fn paginate<'db, C, P>(
    query: P,
    db: &'db C,
    params: &PaginationParams,
) -> WebResult<Paginated<Vec<<P::Selector as SelectorTrait>::Item>>>
where
    C: ConnectionTrait,
    P: PaginatorTrait<'db, C>,
{
    let page = params.page();
    let per_page = params.per_page();
    let paginator = query.paginate(db, per_page);
    let total = paginator.num_items().await?;
    let items = paginator.fetch_page(page - 1).await?;

    Ok(Paginated {
        items,
        total,
        page,
        per_page,
    })
}

/// Batch-resolve role ids to their names, returning an id to name map.
pub async fn role_names<C: ConnectionTrait>(
    db: &C,
    role_ids: Vec<RoleId>,
) -> WebResult<HashMap<RoleId, String>> {
    if role_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let map = ERole::find()
        .filter(CRole::Id.is_in(role_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();

    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_json_wraps_with_error_false() {
        let response = ok_json(42i32);
        assert!(!response.0.error);
        assert_eq!(response.0.message, 42);
    }

    #[test]
    fn or_not_found_returns_value_for_some() {
        let r: WebResult<i32> = Some(7).or_not_found("Thing");
        assert_eq!(r.unwrap(), 7);
    }

    #[test]
    fn or_not_found_maps_none_to_not_found() {
        let r: WebResult<i32> = Option::<i32>::None.or_not_found("Thing");
        match r.unwrap_err() {
            WebError::NotFound(_, msg) => assert_eq!(msg, "Thing not found"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
