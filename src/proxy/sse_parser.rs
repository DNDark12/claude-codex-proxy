use anyhow::Result;
use futures_util::StreamExt;
use reqwest::Response;
use serde_json::Value;

use crate::domain::codex::ParsedSseEvent;

pub fn parse_sse_response(
    response: Response,
) -> impl futures_core::Stream<Item = Result<ParsedSseEvent>> + Send {
    async_stream::try_stream! {
        let mut buffer = String::new();
        let mut stream = response.bytes_stream();

        while let Some(item) = stream.next().await {
            let chunk = item?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some((block, remaining)) = split_next_block(&buffer) {
                buffer = remaining;
                if let Some(parsed) = parse_sse_block(&block)? {
                    yield parsed;
                }
            }
        }

        if !buffer.trim().is_empty() {
            if let Some(parsed) = parse_sse_block(&buffer)? {
                yield parsed;
            }
        }
    }
}

fn split_next_block(input: &str) -> Option<(String, String)> {
    if let Some(pos) = input.find("\n\n") {
        let block = input[..pos].to_string();
        let rest = input[pos + 2..].to_string();
        return Some((block, rest));
    }

    if let Some(pos) = input.find("\r\n\r\n") {
        let block = input[..pos].to_string();
        let rest = input[pos + 4..].to_string();
        return Some((block, rest));
    }

    None
}

fn parse_sse_block(block: &str) -> Result<Option<ParsedSseEvent>> {
    let mut event_name: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();

    for raw_line in block.lines() {
        let line = raw_line.trim_end_matches('\r');

        if let Some(rest) = line.strip_prefix("event:") {
            event_name = Some(rest.trim().to_string());
            continue;
        }

        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start().to_string());
            continue;
        }
    }

    if data_lines.is_empty() {
        return Ok(None);
    }

    let raw_data = data_lines.join("\n");
    if raw_data.trim() == "[DONE]" {
        return Ok(Some(ParsedSseEvent::Done));
    }

    let payload: Value = match serde_json::from_str(raw_data.trim()) {
        Ok(json) => json,
        Err(_) => Value::String(raw_data),
    };

    Ok(Some(ParsedSseEvent::Json {
        event: event_name,
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_chunks(chunks: &[&str]) -> Vec<ParsedSseEvent> {
        let mut out = Vec::new();
        let mut buffer = String::new();

        for chunk in chunks {
            buffer.push_str(chunk);
            while let Some((block, remaining)) = split_next_block(&buffer) {
                buffer = remaining;
                if let Some(parsed) = parse_sse_block(&block).expect("parser") {
                    out.push(parsed);
                }
            }
        }

        if !buffer.trim().is_empty() {
            if let Some(parsed) = parse_sse_block(&buffer).expect("parser") {
                out.push(parsed);
            }
        }

        out
    }

    #[test]
    fn parses_chunk_split_event() {
        let events = parse_chunks(&[
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n",
            "\n",
        ]);

        assert_eq!(events.len(), 2);
    }

    #[test]
    fn parses_done_marker() {
        let events = parse_chunks(&["data: [DONE]\n\n"]);
        assert!(matches!(events.first(), Some(ParsedSseEvent::Done)));
    }

    #[test]
    fn parses_missing_newline_at_end() {
        let events = parse_chunks(&["data: {\"type\":\"response.completed\"}"]);
        assert_eq!(events.len(), 1);
    }
}
