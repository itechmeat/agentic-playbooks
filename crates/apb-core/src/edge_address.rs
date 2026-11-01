//! The address of one edge inside one playbook definition.
//!
//! An edge has no identity of its own unless its author gave it an `id`, and
//! two edges may share both ends. An address names exactly one edge of one
//! definition: `id:<id>` for an edge with an authored id, else
//! `e:<from>><to>#<ordinal>`, where the ordinal is the 1-based position among
//! the id-less edges with exactly those ends, in declaration order. The ends
//! are percent-encoded outside `[A-Za-z0-9_.-]`, so `>` and `#` never occur
//! inside an encoded end and parsing is exact: equal strings mean equal
//! addresses.
//!
//! An address is not an identity across versions: whether an edge of one
//! version is "the same" as an edge of another is what the version diff's
//! correspondence states (`versioning::diff`), never an equality of strings.
//!
//! The dashboard has a port of this module (`web/src/lib/edgeaddress.ts`);
//! both read `tests/fixtures/edge_addresses.json`, so they cannot drift.

use serde::{Deserialize, Serialize};

use crate::schema::{Edge, Playbook};

/// The longest authored edge id.
pub const MAX_EDGE_ID_LEN: usize = 64;

/// Whether `id` is a well-formed authored edge id: `[a-z0-9][a-z0-9_-]*`, at
/// most [`MAX_EDGE_ID_LEN`] characters.
pub fn is_valid_edge_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_EDGE_ID_LEN
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// One edge's address in one definition.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EdgeAddress {
    /// The edge carries an authored `id`.
    Authored { id: String },
    /// An id-less edge: its ends and its 1-based position among the id-less
    /// edges with exactly those ends.
    Derived {
        from: String,
        to: String,
        ordinal: u32,
    },
}

/// Why a string is not an edge address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an edge address: {0}")]
pub struct AddressError(pub String);

fn is_plain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-'
}

/// Percent-encodes every byte outside `[A-Za-z0-9_.-]` as `%XX` (upper-case
/// hex).
pub fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if is_plain(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The exact inverse of [`encode_component`]: a plain byte that was encoded,
/// lower-case hex, a stray `%` or a byte that must be encoded makes the
/// component invalid, so one address has one string form.
fn decode_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            if !hex
                .iter()
                .all(|h| h.is_ascii_digit() || (b'A'..=b'F').contains(h))
            {
                return None;
            }
            let v = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            if is_plain(v) {
                return None;
            }
            out.push(v);
            i += 3;
        } else if is_plain(b) {
            out.push(b);
            i += 1;
        } else {
            return None;
        }
    }
    String::from_utf8(out).ok()
}

impl EdgeAddress {
    /// The string form: `id:<id>` or `e:<from>><to>#<ordinal>`.
    pub fn render(&self) -> String {
        match self {
            EdgeAddress::Authored { id } => format!("id:{}", encode_component(id)),
            EdgeAddress::Derived { from, to, ordinal } => format!(
                "e:{}>{}#{ordinal}",
                encode_component(from),
                encode_component(to)
            ),
        }
    }

    /// Parses the string form exactly; anything [`render`](Self::render)
    /// would not produce is refused.
    pub fn parse(s: &str) -> Result<Self, AddressError> {
        let bad = || AddressError(s.to_string());
        if let Some(id) = s.strip_prefix("id:") {
            let id = decode_component(id)
                .filter(|i| !i.is_empty())
                .ok_or_else(bad)?;
            return Ok(EdgeAddress::Authored { id });
        }
        let rest = s.strip_prefix("e:").ok_or_else(bad)?;
        let (ends, ordinal) = rest.rsplit_once('#').ok_or_else(bad)?;
        let (from, to) = ends.split_once('>').ok_or_else(bad)?;
        if ordinal.is_empty()
            || ordinal.starts_with('0')
            || !ordinal.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(bad());
        }
        let ordinal: u32 = ordinal.parse().map_err(|_| bad())?;
        let from = decode_component(from)
            .filter(|f| !f.is_empty())
            .ok_or_else(bad)?;
        let to = decode_component(to)
            .filter(|t| !t.is_empty())
            .ok_or_else(bad)?;
        Ok(EdgeAddress::Derived { from, to, ordinal })
    }
}

impl std::fmt::Display for EdgeAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

/// Where an edge stands in its source's routing, for display: unconditional
/// edges fan out together, conditional ones are checked in declaration order,
/// and a fallback is taken only when nothing else matched (the predicates of
/// the engine's edge selection).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "n", rename_all = "snake_case")]
pub enum CheckOrder {
    /// An unconditional, non-fallback edge: taken with every other one.
    Parallel,
    /// A conditional, non-fallback edge: checked as the n-th (1-based) of its
    /// source's conditional edges.
    Ordered(u32),
    /// A fallback edge.
    Fallback,
}

impl CheckOrder {
    /// The check order of `edge` given how many conditional, non-fallback
    /// edges of its source come before it.
    fn of(edge: &Edge, conditional_before: u32) -> Self {
        if edge.fallback {
            CheckOrder::Fallback
        } else if edge.condition.is_some() {
            CheckOrder::Ordered(conditional_before + 1)
        } else {
            CheckOrder::Parallel
        }
    }
}

/// One edge of a definition with its address.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressedEdge {
    pub address: EdgeAddress,
    /// [`EdgeAddress::render`] of `address`, for a flat key.
    pub rendered: String,
    /// The position in the playbook's `edges`.
    pub index: usize,
    /// The position among the source's outgoing edges (the rank a judge
    /// edge's answer is kept under).
    pub rank: usize,
    pub check: CheckOrder,
}

/// The address of every edge of `pb`, aligned with `pb.edges`.
pub fn edge_addresses(pb: &Playbook) -> Vec<AddressedEdge> {
    edges_addressed(&pb.edges)
}

/// [`edge_addresses`] over a bare edge list.
pub fn edges_addressed(edges: &[Edge]) -> Vec<AddressedEdge> {
    use std::collections::HashMap;
    let mut ordinals: HashMap<(&str, &str), u32> = HashMap::new();
    let mut ranks: HashMap<&str, usize> = HashMap::new();
    let mut conditional: HashMap<&str, u32> = HashMap::new();
    edges
        .iter()
        .enumerate()
        .map(|(index, e)| {
            let address = match &e.id {
                Some(id) => EdgeAddress::Authored { id: id.clone() },
                None => {
                    let n = ordinals.entry((&e.from, &e.to)).or_insert(0);
                    *n += 1;
                    EdgeAddress::Derived {
                        from: e.from.clone(),
                        to: e.to.clone(),
                        ordinal: *n,
                    }
                }
            };
            let rank = ranks.entry(&e.from).or_insert(0);
            let this_rank = *rank;
            *rank += 1;
            let before = conditional.entry(&e.from).or_insert(0);
            let check = CheckOrder::of(e, *before);
            if matches!(check, CheckOrder::Ordered(_)) {
                *before += 1;
            }
            AddressedEdge {
                rendered: address.render(),
                address,
                index,
                rank: this_rank,
                check,
            }
        })
        .collect()
}

/// The rendered address of the edge at `index` of `pb.edges`.
pub fn rendered_at(pb: &Playbook, index: usize) -> Option<String> {
    edge_addresses(pb)
        .into_iter()
        .nth(index)
        .map(|a| a.rendered)
}

/// The index in `pb.edges` of the edge `address` names, or `None`. An id
/// used twice (a validation error) resolves to its first edge.
pub fn resolve(pb: &Playbook, address: &EdgeAddress) -> Option<usize> {
    edge_addresses(pb)
        .into_iter()
        .find(|a| &a.address == address)
        .map(|a| a.index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_ids_follow_the_grammar() {
        assert!(is_valid_edge_id("a"));
        assert!(is_valid_edge_id("0-retry_x"));
        assert!(!is_valid_edge_id(""));
        assert!(!is_valid_edge_id("-a"));
        assert!(!is_valid_edge_id("_a"));
        assert!(!is_valid_edge_id("A"));
        assert!(!is_valid_edge_id("a.b"));
        assert!(is_valid_edge_id(&"a".repeat(64)));
        assert!(!is_valid_edge_id(&"a".repeat(65)));
    }
}
