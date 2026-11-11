use serde_json::Value;

/// Reads provider-reported cumulative token usage from arbitrarily split SSE frames.
#[derive(Default)]
pub struct ChatUsage {
  line: Vec<u8>,
  data: Vec<u8>,
  total: i64,
}

impl ChatUsage {
  pub fn push(&mut self, bytes: &[u8]) -> i64 {
    let before = self.total;
    for &byte in bytes {
      if byte != b'\n' {
        // ponytail: cap one provider event at 1 MiB; no full-answer buffering.
        if self.line.len() < 1024 * 1024 {
          self.line.push(byte);
        }
        continue;
      }
      if self.line.last() == Some(&b'\r') {
        self.line.pop();
      }
      if self.line.is_empty() {
        if let Ok(value) = serde_json::from_slice::<Value>(&self.data) {
          let usage = &value["usage"];
          let tokens = usage["total_tokens"].as_i64().or_else(|| {
            let input = usage["prompt_tokens"]
              .as_i64()
              .or_else(|| usage["input_tokens"].as_i64())?;
            let output = usage["completion_tokens"]
              .as_i64()
              .or_else(|| usage["output_tokens"].as_i64())?;
            (input >= 0 && output >= 0)
              .then(|| input.checked_add(output))
              .flatten()
          });
          if let Some(tokens) = tokens.filter(|tokens| *tokens >= 0) {
            self.total = self.total.max(tokens);
          }
        }
        self.data.clear();
      } else if let Some(data) = self.line.strip_prefix(b"data:") {
        if self.data.len() + data.len() < 1024 * 1024 {
          self.data.extend_from_slice(data);
          self.data.push(b'\n');
        }
      }
      self.line.clear();
    }
    self.total - before
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn split_frames_and_repeated_cumulative_usage_are_counted_once() {
    let mut usage = ChatUsage::default();
    let stream = b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":8}}\r\n\r\ndata: {\"usage\":{\"total_tokens\":20}}\n\ndata: {\"usage\":{\"total_tokens\":25}}\n\ndata: [DONE]\n\n";
    let tokens: i64 = stream.chunks(3).map(|bytes| usage.push(bytes)).sum();
    assert_eq!(tokens, 25);
    assert_eq!(
      usage.push(b"data: {\"usage\":{\"total_tokens\":-100}}\n\n"),
      0
    );
    assert_eq!(
      usage.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n"),
      0
    );
  }
}
