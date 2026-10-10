/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_entity::build_log_chunk::{Column, Entity};
use gradient_entity::ids::BuildAttemptId;
use gradient_storage::LogStorage;
use gradient_storage::log_chunk::{ChunkSpan, read_chunks_from};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};

#[derive(Default)]
pub struct LogTail {
    attempt: Option<BuildAttemptId>,
    offset: u64,
}

impl LogTail {
    pub fn after(attempt: Option<BuildAttemptId>, offset: u64) -> Self {
        Self { attempt, offset }
    }

    pub async fn appended(
        &mut self,
        db: &impl ConnectionTrait,
        logs: &dyn LogStorage,
        attempt: BuildAttemptId,
    ) -> Result<String> {
        if self.attempt != Some(attempt) {
            *self = Self::after(Some(attempt), 0);
        }

        let Some(inline) = logs.read_inline_from(attempt, self.offset).await? else {
            let chunks = chunk_spans(db, attempt).await?;
            let (text, end) = read_chunks_from(logs, attempt, &chunks, self.offset).await?;
            self.offset = end;

            return Ok(text);
        };

        let (text, consumed) = complete_utf8(&inline);
        self.offset += consumed as u64;

        Ok(text)
    }
}

async fn chunk_spans(db: &impl ConnectionTrait, attempt: BuildAttemptId) -> Result<Vec<ChunkSpan>> {
    Ok(Entity::find()
        .filter(Column::BuildAttempt.eq(attempt))
        .order_by_asc(Column::ChunkIndex)
        .all(db)
        .await?
        .into_iter()
        .map(|row| ChunkSpan {
            index: row.chunk_index as u32,
            byte_start: row.byte_start as u64,
            byte_len: row.byte_len as u32,
            line_count: row.line_count.max(0) as u32,
        })
        .collect())
}

fn complete_utf8(bytes: &[u8]) -> (String, usize) {
    let complete = match std::str::from_utf8(bytes) {
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        _ => bytes.len(),
    };

    (
        String::from_utf8_lossy(&bytes[..complete]).into_owned(),
        complete,
    )
}

#[cfg(test)]
mod tests {
    use super::complete_utf8;

    #[test]
    fn complete_utf8_holds_back_a_split_character() {
        let cases: [(&str, &[u8], (&str, usize)); 4] = [
            ("whole text", "ok\n".as_bytes(), ("ok\n", 3)),
            ("split last character", &[b'a', 0xC3], ("a", 1)),
            ("invalid byte", &[b'a', 0xFF, b'b'], ("a\u{FFFD}b", 3)),
            ("nothing", &[], ("", 0)),
        ];

        for (case, bytes, (text, consumed)) in cases {
            assert_eq!(complete_utf8(bytes), (text.to_owned(), consumed), "{case}");
        }
    }
}
