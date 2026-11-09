//! dsh's `--json` stdout: a `tool_call` event carries its call's facts,
//! decoded by [`apb_core::agent_actions::dsh`], and a `tool_result` with
//! status `error` is a failed call.

use super::events::{self, Decoder};
use super::record::Record;

const DECODER: Decoder = Decoder {
    calls: apb_core::agent_actions::dsh::calls_of_line,
    failed: apb_core::agent_actions::dsh::failed_result,
};

/// The records of a dsh stdout, and whether every line parsed.
pub fn parse(text: &str) -> (Vec<Record>, bool) {
    events::parse(text, &DECODER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_call_line_carries_its_facts_and_an_error_result_counts() {
        let text = concat!(
            r#"{"type":"session","sessionId":"s","cwd":"/w"}"#,
            "\n",
            r#"{"type":"tool_call","callId":"c","tool":"write","input":{"file_path":"a.md","content":"x"}}"#,
            "\n",
            r#"{"type":"tool_result","callId":"c","status":"error","result":""}"#,
            "\n",
            r#"{"type":"final","sessionId":"s","reason":{"kind":"completed"},"text":"ok"}"#,
            "\n",
        );
        let (records, whole) = parse(text);
        assert!(whole);
        assert_eq!(records.len(), 4);
        assert_eq!(records[1].tools.len(), 1);
        assert!(records[1].tools[0].writes);
        assert_eq!(records[1].tools[0].paths, ["a.md"]);
        assert_eq!(records[2].tool_errors, 1);
        assert_eq!(records[3].text, text.lines().nth(3).unwrap());
    }
}
