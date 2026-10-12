/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use super::lock;

pub trait DownloadSink: Send + Sync {
    fn transfer(&self, id: u64, done: u64, expected: u64);
}

pub type DownloadSlot = Arc<Mutex<Option<Arc<dyn DownloadSink>>>>;

const FILE_TRANSFER: u64 = 101;
const PROGRESS: u64 = 105;

#[derive(Default)]
struct NixLogParser {
    transfers: HashMap<u64, Arc<dyn DownloadSink>>,
}

impl NixLogParser {
    fn feed(&mut self, line: &str, downloads: &DownloadSlot) -> Option<String> {
        let Some(event) = line
            .strip_prefix("@nix ")
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        else {
            return Some(line.to_owned());
        };
        let id = event["id"].as_u64().unwrap_or_default();
        match event["action"].as_str() {
            Some("start") if event["type"].as_u64() == Some(FILE_TRANSFER) => {
                if let Some(sink) = lock(downloads).clone() {
                    self.transfers.insert(id, sink);
                }
            }
            Some("stop") => {
                self.transfers.remove(&id);
            }
            Some("result") if event["type"].as_u64() == Some(PROGRESS) => {
                if let Some(sink) = self.transfers.get(&id) {
                    let field = |i: usize| event["fields"][i].as_u64().unwrap_or_default();
                    sink.transfer(id, field(0), field(1));
                }
            }
            Some("msg") => return Some(event["msg"].as_str().unwrap_or_default().to_owned()),
            _ => {}
        }
        None
    }
}

pub async fn forward_nix_log(
    mut reader: impl AsyncBufRead + Unpin,
    downloads: DownloadSlot,
    mut print: impl FnMut(&str),
) {
    let mut parser = NixLogParser::default();
    let mut line = Vec::new();
    while matches!(reader.read_until(b'\n', &mut line).await, Ok(read) if read > 0) {
        let text = String::from_utf8_lossy(&line);
        if let Some(text) = parser.feed(text.trim_end_matches(['\r', '\n']), &downloads) {
            print(&text);
        }
        line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        transfers: Mutex<Vec<(u64, u64, u64)>>,
    }

    impl DownloadSink for RecordingSink {
        fn transfer(&self, id: u64, done: u64, expected: u64) {
            lock(&self.transfers).push((id, done, expected));
        }
    }

    fn slot_on(sink: &Arc<RecordingSink>) -> DownloadSlot {
        Arc::new(Mutex::new(Some(Arc::clone(sink) as Arc<dyn DownloadSink>)))
    }

    fn transfers(sink: &RecordingSink) -> Vec<(u64, u64, u64)> {
        lock(&sink.transfers).clone()
    }

    #[test]
    fn only_file_transfer_activities_count() {
        let sink = Arc::new(RecordingSink::default());
        let slot = slot_on(&sink);
        let mut parser = NixLogParser::default();
        let lines = [
            r#"@nix {"action":"start","id":7,"type":101,"text":"downloading"}"#,
            r#"@nix {"action":"start","id":8,"type":100,"text":"copying"}"#,
            r#"@nix {"action":"result","id":7,"type":105,"fields":[512,2048,0,0]}"#,
            r#"@nix {"action":"result","id":8,"type":105,"fields":[1,1,0,0]}"#,
        ];
        for line in lines {
            assert_eq!(parser.feed(line, &slot), None);
        }
        assert_eq!(transfers(&sink), vec![(7, 512, 2048)]);
        assert_eq!(
            parser.feed(
                r#"@nix {"action":"msg","level":1,"msg":"warning: x"}"#,
                &slot
            ),
            Some("warning: x".into())
        );
        assert_eq!(
            parser.feed("plain tracing line", &slot),
            Some("plain tracing line".into())
        );
    }

    #[test]
    fn a_transfer_stays_with_the_sink_it_started_on() {
        let first = Arc::new(RecordingSink::default());
        let second = Arc::new(RecordingSink::default());
        let mut parser = NixLogParser::default();
        parser.feed(
            r#"@nix {"action":"start","id":7,"type":101}"#,
            &slot_on(&first),
        );
        parser.feed(
            r#"@nix {"action":"result","id":7,"type":105,"fields":[40,90]}"#,
            &slot_on(&second),
        );
        assert_eq!(transfers(&first), vec![(7, 40, 90)]);
        assert!(transfers(&second).is_empty());

        parser.feed(r#"@nix {"action":"stop","id":7}"#, &slot_on(&second));
        parser.feed(
            r#"@nix {"action":"result","id":7,"type":105,"fields":[90,90]}"#,
            &slot_on(&second),
        );
        assert_eq!(transfers(&first), vec![(7, 40, 90)]);
        assert!(transfers(&second).is_empty());
    }

    #[tokio::test]
    async fn a_line_that_is_not_utf8_does_not_end_the_log() {
        let sink = Arc::new(RecordingSink::default());
        let downloads = slot_on(&sink);
        let mut log = b"trace: \xff\xfe\n".to_vec();
        log.extend_from_slice(b"@nix {\"action\":\"start\",\"id\":7,\"type\":101}\n");
        log.extend_from_slice(
            b"@nix {\"action\":\"result\",\"id\":7,\"type\":105,\"fields\":[3,9]}\n",
        );
        let mut printed = Vec::new();
        forward_nix_log(log.as_slice(), downloads, |text| {
            printed.push(text.to_owned())
        })
        .await;
        assert_eq!(printed, vec!["trace: \u{fffd}\u{fffd}".to_owned()]);
        assert_eq!(transfers(&sink), vec![(7, 3, 9)]);
    }
}
