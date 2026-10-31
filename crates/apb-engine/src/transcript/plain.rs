//! A stream without a structure apb knows (stdout, stderr, an agent other
//! than claude): one record per line, no tool calls.

use super::record::Record;

/// The records of `text`, one per line.
pub fn parse(text: &str) -> Vec<Record> {
    text.lines()
        .enumerate()
        .map(|(i, line)| Record {
            index: i as u64,
            text: line.to_string(),
            ..Record::default()
        })
        .collect()
}
