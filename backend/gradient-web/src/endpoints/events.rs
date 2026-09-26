/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `GET /metrics/events`: every event on this server instance as JSON lines, for
//! debugging. Best-effort and lossy by design; durable consumers use webhooks.

use super::live::live_stream;
use crate::error::{WebResult, require_superuser};
use crate::helpers::ok_json;
use axum::extract::ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::events::{CatalogEntry, Envelope, Event, EventFilter};
use gradient_types::{BaseResponse, MUser};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize, Debug, Default)]
pub struct FirehoseQuery {
    pub events: Option<String>,
}

/// The upgrade is checked after the caller, so a non-superuser learns 403 rather than 426.
pub async fn firehose_ws(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(query): Query<FirehoseQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> WebResult<Response> {
    require_superuser(&user)?;
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return Ok(rejection.into_response()),
    };
    let rx = state.events.subscribe();
    let select = select_frames(EventFilter::parse(query.events.as_deref()));
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        let _ = shutdown
            .spawn(live_stream(socket, rx, select, lag_frame, cancel))
            .await;
    }))
}

fn select_frames(filter: EventFilter) -> impl Fn(&Envelope) -> Option<String> + Send + 'static {
    move |env| filter.matches(&env.event.name()).then(|| env.to_line())
}

fn lag_frame(skipped: u64) -> Option<String> {
    Some(
        serde_json::json!({
            "event": "stream.lagged",
            "at": chrono::Utc::now(),
            "content": { "skipped": skipped },
        })
        .to_string(),
    )
}

pub async fn get_catalog(
    Extension(_user): Extension<MUser>,
) -> Json<BaseResponse<Vec<CatalogEntry>>> {
    ok_json(Event::catalog())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::events::{Envelope, cache, worker};

    #[test]
    fn the_filter_selects_frames() {
        let select = select_frames(EventFilter::parse(Some("worker.*")));
        let depth = worker::QueueDepth {
            workers: 1,
            pending: 0,
            active: 0,
        };
        assert!(select(&Envelope::now(depth.into())).is_some());
        assert!(select(&Envelope::now(cache::Changed {}.into())).is_none());
    }

    #[test]
    fn a_lagged_subscriber_gets_a_lag_frame_and_continues() {
        let frame: serde_json::Value = serde_json::from_str(&lag_frame(7).unwrap()).unwrap();
        assert_eq!(frame["event"], "stream.lagged");
        assert_eq!(frame["content"]["skipped"], 7);
    }
}
