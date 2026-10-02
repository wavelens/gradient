/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use axum::Json;
use gradient_types::ids::RoleId;
use gradient_types::{BaseResponse, CRole, ERole, Paginated, PaginationParams};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, SelectorTrait,
};
use std::collections::HashMap;

#[inline]
pub fn ok_json<T>(message: T) -> Json<BaseResponse<T>> {
    Json(BaseResponse {
        error: false,
        message,
    })
}

pub trait OptionExt<T> {
    fn or_not_found(self, resource: &str) -> WebResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn or_not_found(self, resource: &str) -> WebResult<T> {
        self.ok_or_else(|| WebError::not_found(resource))
    }
}

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
