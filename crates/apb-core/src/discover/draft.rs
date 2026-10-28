//! A repeated pattern as a suggestion: a title, the synopsis
//! `playbook_capture` takes, a schema 2 playbook draft, the effects its
//! steps have, scrubbed examples of what a person asked for right before
//! it, and the places it was seen.
//!
//! An example is the last request typed before an occurrence's first step,
//! dropped whole when [`crate::scrub::Scrubber::refuses`] flags it before
//! or after the scrub, else kept through
//! [`crate::scrub::Scrubber::stored_text`] of its project, with every
//! absolute path left in it (one the scrub does not rewrite) as `[path]`.
//! A draft whose synopsis or yaml a project's scrubber refuses is dropped.
//!
//! The draft names the first profile of the first project the repeat was
//! seen in; [`retarget`] names the profile of the place it is saved in
//! instead.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::mine::Pattern;
use super::session::Session;
use super::token;
use super::types::{
    Example, HistoryRoot, ProjectRef, SourceSpan, Suggestion, SuggestionStatus, Support,
};
use crate::scrub::Scrubber;
use crate::validate::nodes::TRIGGER_MAX_LEN;

/// The longest example kept, in characters.
pub const EXAMPLE_MAX_CHARS: usize = 240;
/// Examples kept per suggestion, from distinct sessions.
pub const MAX_EXAMPLES: usize = 3;
/// Sources kept per suggestion.
pub const MAX_SOURCES: usize = 10;
/// The profile a draft names when its project has none.
pub const FALLBACK_PROFILE: &str = "main";

/// What drafting reads, per request.
pub struct Context<'a> {
    pub sessions: &'a [Session],
    pub projects: &'a [ProjectRef],
    pub roots: &'a [HistoryRoot],
    /// One per project.
    pub scrubbers: Vec<Scrubber>,
    /// Each project's first profile.
    pub profiles: Vec<Option<String>>,
}

/// An absolute path in typed text: a `/`-rooted or a drive-letter path at
/// the start, after a space, a quote, a backtick, a bracket, `=`, `:`, `,`
/// or a `file://` scheme. A `//` after `:` is the authority of a URL
/// (`https://host/x`), not a path.
static ABSOLUTE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(^|file://|[\s"'`(=<\[{:,])(/[^/\s"'`()<>\]][^\s"'`()<>\]]*|[A-Za-z]:\\[^\s"'`()<>\]]*)"#,
    )
    .expect("valid")
});

/// `text` with every absolute path as `[path]`.
fn without_absolute_paths(text: &str) -> String {
    ABSOLUTE_PATH.replace_all(text, "${1}[path]").into_owned()
}

impl Context<'_> {
    fn span(&self, session: &Session, lines: [u32; 2]) -> SourceSpan {
        SourceSpan {
            root: self.roots[session.root].name.clone(),
            file: session.rel.clone(),
            workspace_id: self.projects[session.project].workspace_id.clone(),
            lines,
        }
    }

    /// The example of one occurrence: the last request before `line`.
    fn example(&self, session: &Session, line: u32) -> Option<Example> {
        let (at, text) = session.requests.iter().rev().find(|(l, _)| *l < line)?;
        let scrubber = &self.scrubbers[session.project];
        // Stored text is refused rather than rewritten: a request that held
        // anything secret-like before the scrub is dropped whole.
        if scrubber.refuses(text).is_some() {
            return None;
        }
        let text = scrubber
            .stored_text(text, EXAMPLE_MAX_CHARS)
            .map(|t| without_absolute_paths(&t))
            .filter(|t| scrubber.refuses(t).is_none())?;
        Some(Example {
            text,
            source: self.span(session, [*at, *at]),
        })
    }
}

/// The suggestion of `p`; `None` when its draft still looks like it holds
/// a secret or does not serialise.
pub fn suggestion(p: &Pattern, ctx: &Context<'_>) -> Option<Suggestion> {
    let labels: Vec<String> = p.tokens.iter().map(|t| token::label(t)).collect();
    let title = format!("Repeated steps: {}", labels.join(", "));
    // The most recent sessions first.
    let mut occ = p.occurrences.clone();
    occ.sort_by_key(|o| std::cmp::Reverse((ctx.sessions[o.session].day, o.session)));
    let mut examples = Vec::new();
    let mut sources = Vec::new();
    let mut destructive = false;
    let mut projects: Vec<usize> = Vec::new();
    for o in &occ {
        let s = &ctx.sessions[o.session];
        let steps = &s.steps[o.start..=o.end];
        destructive |= steps.iter().any(|st| st.destructive);
        if !projects.contains(&s.project) {
            projects.push(s.project);
        }
        if sources.len() < MAX_SOURCES {
            sources.push(ctx.span(s, [steps[0].line, steps[steps.len() - 1].line]));
        }
        // The same request typed in several sessions is shown once.
        if examples.len() < MAX_EXAMPLES
            && let Some(e) = ctx.example(s, steps[0].line)
            && !examples.iter().any(|x: &Example| x.text == e.text)
        {
            examples.push(e);
        }
    }
    projects.sort_unstable();
    let mut effects: Vec<String> = token::effects(&p.tokens)
        .into_iter()
        .map(str::to_string)
        .collect();
    if destructive {
        effects.push("irreversible".to_string());
    }
    let example_texts = trigger_examples(&examples);
    let when = trigger_line(
        &format!("the task takes these steps in order: {}", labels.join(", ")),
        &p.slug,
    );
    let synopsis = json!({
        "title": title,
        "trigger": { "when": [when], "examples": example_texts },
        "steps": labels,
    });
    let profile = projects.iter().find_map(|i| ctx.profiles[*i].clone());
    let mut notes = Vec::new();
    if profile.is_none() {
        notes.push("needs_profile".to_string());
    }
    let profile = profile.unwrap_or_else(|| FALLBACK_PROFILE.to_string());
    let yaml = draft_yaml(&p.slug, &title, &when, &example_texts, &labels, &profile)?;
    let refused = projects.iter().any(|i| {
        let s = &ctx.scrubbers[*i];
        s.refuses_value(&synopsis).is_some() || s.refuses(&yaml).is_some()
    });
    if refused {
        return None;
    }
    Some(Suggestion {
        slug: p.slug.clone(),
        steps: p.tokens.clone(),
        support: Support {
            sessions: p.sessions.len() as u32,
            days: p.days,
            projects: projects.iter().map(|i| ctx.projects[*i].seen()).collect(),
        },
        score: p.score,
        title,
        synopsis,
        yaml,
        effects,
        examples,
        sources,
        notes,
        status: SuggestionStatus::New,
    })
}

/// A trigger line as the validator takes it: at most
/// [`TRIGGER_MAX_LEN`] characters. A longer one is cut after the last
/// whole list item that fits beside its `slug` (`... git commit, and more
/// (seq-...)`): two repeats that share their first steps keep distinct
/// triggers, so saving one never makes the other a duplicate.
fn trigger_line(s: &str, slug: &str) -> String {
    if s.chars().count() <= TRIGGER_MAX_LEN {
        return s.to_string();
    }
    let suffix = format!(", and more ({slug})");
    let room = TRIGGER_MAX_LEN.saturating_sub(suffix.chars().count());
    let head: String = s.chars().take(room).collect();
    let head = match head.rfind(", ") {
        Some(at) => &head[..at],
        None => head.as_str(),
    };
    format!("{head}{suffix}")
}

/// The trigger examples of a draft: each example's text cut to
/// [`TRIGGER_MAX_LEN`] characters.
fn trigger_examples(examples: &[Example]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in examples {
        let t: String = e.text.chars().take(TRIGGER_MAX_LEN).collect();
        let t = t.trim().to_string();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// The playbook draft: a `task` parameter and one agent step that takes
/// the steps in order.
fn draft_yaml(
    id: &str,
    title: &str,
    when: &str,
    examples: &[String],
    labels: &[String],
    profile: &str,
) -> Option<String> {
    let steps: String = labels
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}. {l}\n", i + 1))
        .collect();
    let prompt = format!("Task: {{{{params.task}}}}\n\nDo it in these steps, in order:\n{steps}");
    let doc: Value = json!({
        "schema": 2,
        "id": id,
        "name": title,
        "description": "Drafted from steps repeated in past agent sessions.",
        "version": "0.1.0",
        "trigger": { "when": [when], "examples": examples },
        "params": [{ "name": "task", "type": "text", "label": "Task" }],
        "defaults": { "profile": profile },
        "nodes": [
            { "id": "start", "type": "start" },
            { "id": "work", "type": "agent_task", "title": title, "prompt": prompt },
            { "id": "done", "type": "finish", "outcome": "success" },
        ],
        "edges": [
            { "from": "start", "to": "work" },
            { "from": "work", "to": "done" },
        ],
    });
    yaml_serde::to_string(&doc).ok()
}

/// The draft `yaml` naming `profile` as its default profile; `None` when
/// it is not a mapping that reads and writes back.
pub fn with_profile(yaml: &str, profile: &str) -> Option<String> {
    let mut doc: Value = yaml_serde::from_str(yaml).ok()?;
    let defaults = doc.as_object_mut()?.entry("defaults").or_insert(json!({}));
    defaults
        .as_object_mut()?
        .insert("profile".into(), Value::String(profile.to_string()));
    yaml_serde::to_string(&doc).ok()
}

/// The first profile, by name, of the profile store under `parent` (a
/// project's `.apb` or the global store's directory).
pub fn first_profile(parent: &std::path::Path) -> Option<String> {
    crate::registry::Registry::open_dir(parent)
        .ok()?
        .profiles()
        .into_iter()
        .next()
}

/// A suggestion's own draft as it is saved: naming the first profile of
/// the first of `stores` (the profile stores the saved playbook resolves
/// its profile in, in that order) that has one, else as it was drafted.
pub fn retarget(yaml: &str, stores: &[&std::path::Path]) -> String {
    stores
        .iter()
        .find_map(|s| first_profile(s))
        .and_then(|profile| with_profile(yaml, &profile))
        .unwrap_or_else(|| yaml.to_string())
}

#[cfg(test)]
mod tests {
    use super::{trigger_line, without_absolute_paths};
    use crate::validate::nodes::TRIGGER_MAX_LEN;

    #[test]
    fn a_cut_trigger_line_keeps_the_slug_so_shared_prefixes_stay_apart() {
        let steps = "the task takes these steps in order: cargo test, edit .rs, cargo test, git commit, mcp notion search, git push";
        let a = trigger_line(&format!("{steps}, web fetch"), "seq-aaaaaaaaaa");
        let b = trigger_line(&format!("{steps}, agent spawn"), "seq-bbbbbbbbbb");
        assert_ne!(a, b);
        for line in [&a, &b] {
            assert!(line.chars().count() <= TRIGGER_MAX_LEN, "{line}");
            assert!(line.contains(", and more (seq-"), "{line}");
        }
        // A line that fits is kept whole, without the slug.
        assert_eq!(
            trigger_line("the task takes these steps in order: a, b", "seq-x"),
            "the task takes these steps in order: a, b"
        );
    }

    #[test]
    fn an_absolute_path_is_hidden_wherever_it_starts() {
        for (typed, kept) in [
            ("see `/srv/box/x` now", "see `[path]` now"),
            (
                "file:/home/u/x and file:///home/u/y",
                "file:[path] and file://[path]",
            ),
            // A path runs to the next space: what follows it goes too.
            ("a,/home/u,b {/opt/x}", "a,[path] {[path]"),
            ("in C:\\Users\\u\\x", "in [path]"),
            (
                "read https://example.com/docs/a",
                "read https://example.com/docs/a",
            ),
            ("a / b and 3/4", "a / b and 3/4"),
        ] {
            assert_eq!(without_absolute_paths(typed), kept, "{typed}");
        }
    }
}
