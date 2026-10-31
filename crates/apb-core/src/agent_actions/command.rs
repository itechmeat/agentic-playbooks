//! Shell commands as actions: a command line split into its simple
//! commands, each named by its program and, for the tools whose
//! subcommand says what they do, that subcommand (`git commit`,
//! `gh pr create`), never by its arguments.
//!
//! A subcommand is named only when it is one the program is known to have
//! ([`super::known`]): any other word, an option's value included, never
//! names a step.
//!
//! The destructive list lives here too ([`is_destructive`]): every reader
//! of an agent's commands (the run transcript, history discovery) asks the
//! same question the same way.

use std::sync::LazyLock;

use regex::Regex;

use super::action::{Action, ToolCall};
use super::known::{self, OTHER};

/// Shell commands that cannot be taken back and read the same in any
/// spelling of their flags: a hard reset, dropping or truncating a database
/// table, formatting a disk and a raw disk write. The commands whose flags
/// come in many spellings (a recursive forced delete, a forced push, a
/// forced clean, a forced branch delete, `find -delete`) are read token by
/// token ([`destructive_words`]).
pub const DESTRUCTIVE: [&str; 4] = [
    r"\bgit\s+reset\s+--hard\b",
    r"(?i)\b(drop|truncate)\s+(table|database|schema)\b",
    r"\bmkfs(\.[a-z0-9]+)?\b",
    r"\bdd\s+[^|;&]*\bof=/dev/",
];

static DESTRUCTIVE_RE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    DESTRUCTIVE
        .iter()
        .map(|p| Regex::new(p).expect("valid"))
        .collect()
});

/// The short flags (`-rf`) and long flags (`--force`) of a command's words.
/// A short group counts only when it is letters alone: `-f"` closing a
/// quoted argument is text, not a flag.
struct Flags<'a> {
    short: String,
    long: Vec<&'a str>,
}

impl<'a> Flags<'a> {
    fn of(words: &[&'a str]) -> Self {
        let mut short = String::new();
        let mut long = Vec::new();
        for w in words {
            if let Some(l) = w.strip_prefix("--") {
                long.push(l.split('=').next().unwrap_or(l));
            } else if let Some(s) = w.strip_prefix('-')
                && s.chars().all(|c| c.is_ascii_alphabetic())
            {
                short.push_str(s);
            }
        }
        Flags { short, long }
    }

    fn has(&self, short: &[char], long: &[&str]) -> bool {
        self.short.chars().any(|c| short.contains(&c)) || self.long.iter().any(|l| long.contains(l))
    }
}

/// The program a word names: its file name, quotes dropped.
fn program_name(w: &str) -> String {
    let w = w.trim_matches(['"', '\'']);
    w.rsplit('/').next().unwrap_or(w).to_string()
}

/// Whether one simple command (no `|`, `;` or `&`) runs a recursive forced
/// delete, a forced push (`--force`, `-f`, `--force-with-lease`, a `+`
/// refspec), a forced clean, a forced branch delete or `find -delete`,
/// wherever in its words the program stands (after `sudo`, inside
/// `find -exec`).
fn destructive_words(words: &[&str]) -> bool {
    (0..words.len()).any(|at| {
        let rest = &words[at + 1..];
        match program_name(words[at]).as_str() {
            "rm" => {
                let f = Flags::of(rest);
                f.has(&['r', 'R'], &["recursive"]) && f.has(&['f'], &["force"])
            }
            "find" => rest.contains(&"-delete"),
            "git" => git_destructive(rest),
            _ => false,
        }
    })
}

/// `words` after git's global options (`-C dir`, `-c k=v`, `--git-dir=..`):
/// the subcommand and its arguments. An option git's table does not know
/// is taken as one without a value; [`git_destructive`] then reads one
/// word further when the word left is not a git subcommand.
fn git_subcommand<'w, 'a>(words: &'w [&'a str]) -> &'w [&'a str] {
    let value_options = known::program("git").map_or(&[][..], |g| g.options.value);
    let mut rest = words;
    while let Some((w, tail)) = rest.split_first() {
        if !w.starts_with('-') {
            break;
        }
        rest = if value_options.contains(w) {
            tail.get(1..).unwrap_or(&[])
        } else {
            tail
        };
    }
    rest
}

/// Whether the words after `git` force a push, a clean or a branch delete.
/// Global options before the subcommand are skipped. When the word after
/// them is not a git subcommand, it may be the value of an option the
/// table does not know (`git --frob x push --force`): the subcommand is
/// then the next word.
fn git_destructive(words: &[&str]) -> bool {
    let rest = git_subcommand(words);
    let is_sub = |w: &str| known::program("git").is_some_and(|g| g.subs.contains(&w));
    let at = match rest {
        [first, next, ..] if !is_sub(first) && !next.starts_with('-') => 1,
        _ => 0,
    };
    let Some((sub, args)) = rest[at.min(rest.len())..].split_first() else {
        return false;
    };
    let f = Flags::of(args);
    match *sub {
        "push" => {
            f.has(&['f'], &["force", "force-with-lease"]) || args.iter().any(|a| a.starts_with('+'))
        }
        "clean" => f.has(&['f'], &["force"]),
        "branch" => {
            f.short.contains('D') || (f.has(&['d'], &["delete"]) && f.has(&['f'], &["force"]))
        }
        _ => false,
    }
}

/// Whether `text` names a destructive command as written: one of
/// [`DESTRUCTIVE`] anywhere in it (a `DROP TABLE` inside a quoted SQL
/// script too), or a part [`destructive_words`] flags, its words split on
/// whitespace as they stand. A flag inside a quoted argument keeps its
/// quote (`-m "... git push --force"` is a commit message), and a quoted
/// shell script (`bash -lc 'git push --force'`) is read by
/// [`is_destructive`] through the commands it runs.
fn destructive_text(text: &str) -> bool {
    DESTRUCTIVE_RE.iter().any(|re| re.is_match(text))
        || text
            .split(['|', ';', '&', '\n'])
            .any(|part| destructive_words(&part.split_whitespace().collect::<Vec<_>>()))
}

/// Whether `command` runs a destructive command: as written
/// ([`destructive_text`]), or in any simple command it runs, a shell's
/// `-c` script included. The one answer every reader gets: a call's flag
/// and its actions' flags never disagree.
pub fn is_destructive(command: &str) -> bool {
    destructive_text(command) || actions(command).iter().any(Action::is_destructive)
}

/// Words that only move around or print: they say nothing about the work.
const NAVIGATION: [&str; 5] = ["cd", "ls", "pwd", "echo", "true"];

/// Programs that print a file: one of a `SKILL.md` reads that skill.
const FILE_READERS: [&str; 5] = ["cat", "sed", "head", "less", "tail"];

/// Shells whose `-c` script is the command that runs.
const SHELLS: [&str; 4] = ["bash", "sh", "zsh", "dash"];

/// Recursion bound of `bash -c "bash -c ..."`.
const MAX_NESTING: u32 = 4;

/// The simple commands of a command line: split on `&&`, `||`, `;`, `|`,
/// `&` and newlines outside quotes; the `&` of a redirection stays.
fn simple_commands(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match (quote, c) {
            (_, '\\') if quote != Some('\'') => {
                cur.push(c);
                escaped = true;
            }
            (Some(q), c) if c == q => {
                cur.push(c);
                quote = None;
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                cur.push(c);
                quote = Some(c);
            }
            // A redirection (`2>&1`, `>&2`, `&>log`) does not end the
            // command.
            (None, '&') if cur.ends_with(['>', '<']) || chars.peek() == Some(&'>') => cur.push(c),
            (None, '&' | '|' | ';' | '\n') => {
                out.push(std::mem::take(&mut cur));
            }
            (None, c) => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The words of one simple command, quotes resolved; a command that does
/// not split (an open quote) falls back to whitespace.
fn words_of(command: &str) -> Vec<String> {
    shell_words::split(command)
        .unwrap_or_else(|_| command.split_whitespace().map(str::to_string).collect())
}

/// What a simple command runs, once what only wraps it is set aside.
enum Runs<'a> {
    /// The program and its arguments.
    Program(&'a [String]),
    /// A lookup of a name (`command -v git`): nothing runs.
    Lookup,
    /// A wrapper option the table does not know: the next word may be its
    /// value, so the program cannot be told.
    Unknown,
}

/// `words` without what only wraps the program: variable assignments and
/// the wrappers [`known::wrapper`] lists with their options (`sudo -u
/// deploy`, `env -u VAR`, `time -f fmt`).
fn unwrapped(words: &[String]) -> Runs<'_> {
    let mut rest = words;
    while let Some((w, tail)) = rest.split_first() {
        let assignment = w.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if assignment {
            rest = tail;
            continue;
        }
        let name = program_name(w);
        let Some(options) = known::wrapper(&name) else {
            break;
        };
        if name == "command" && tail.first().is_some_and(|f| f == "-v" || f == "-V") {
            return Runs::Lookup;
        }
        rest = match options.lead(tail) {
            known::Lead::At(at) => &tail[at..],
            known::Lead::End => &[],
            known::Lead::Unknown => return Runs::Unknown,
        };
    }
    Runs::Program(rest)
}

/// The skill whose `SKILL.md` `path` is: the name of its directory.
pub fn skill_of_path(path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    (p.file_name()? == "SKILL.md")
        .then(|| p.parent()?.file_name()?.to_str().map(str::to_string))
        .flatten()
}

/// The subcommand of `program` in `args`, for the programs
/// [`known::program`] lists: its first plain word after the options it
/// knows, when that word is a subcommand it lists, else [`OTHER`] (an
/// unknown option first: its value may be next); `gh` and `apb connector`
/// take a second level (`pr.create`).
fn subcommand(program: &str, args: &[&str]) -> Option<String> {
    let known = known::program(program)?;
    let at = match known.options.lead(args) {
        known::Lead::At(at) => at,
        known::Lead::Unknown => return Some(OTHER.to_string()),
        known::Lead::End => return None,
    };
    let first = args[at];
    if !known.subs.contains(&first) {
        return Some(OTHER.to_string());
    }
    let rest = &args[at + 1..];
    let second = known
        .second_level(first)
        .zip(match known::NO_OPTIONS.lead(rest) {
            known::Lead::At(i) => Some(rest[i]),
            _ => None,
        })
        .and_then(|(level, word)| level.contains(&word).then_some(word));
    Some(match second {
        Some(second) => format!("{first}.{second}"),
        None => first.to_string(),
    })
}

/// Programs that edit a file in place under a flag: `sed -i`, `perl -pi`
/// (an `i` in a short flag group before any flag that takes the rest of
/// the group as its value, or `--in-place`), `awk -i inplace`. Only the
/// options before the first operand are read.
fn edits_in_place(program: &str, args: &[&str]) -> bool {
    // The short flags whose value is the rest of their group
    // (`perl -MList::Util`, `sed -e...`).
    let takes_value: &[char] = match program {
        "sed" => &['e', 'f', 'l'],
        "perl" => &['M', 'm', 'I', 'e', 'E', 'x', 'l', '0', 'C', 'd', 'D', 'V'],
        _ => &[],
    };
    let short_i = |a: &str| {
        a.strip_prefix('-')
            .filter(|f| !f.starts_with('-'))
            .is_some_and(|f| {
                f.chars()
                    .take_while(|c| c.is_ascii_alphabetic() && !takes_value.contains(c))
                    .any(|c| c == 'i')
            })
    };
    let options = args.iter().take_while(|a| a.starts_with('-'));
    match program {
        "sed" | "perl" => options
            .copied()
            .any(|a| short_i(a) || a == "--in-place" || a.starts_with("--in-place=")),
        "awk" | "gawk" => args.windows(2).any(|w| w[0] == "-i" && w[1] == "inplace"),
        _ => false,
    }
}

/// The subcommand an in-place edit is named by (`sed:inplace`).
pub const IN_PLACE: &str = "inplace";

/// A call named `name` that runs `line`.
pub fn call(name: &str, input: serde_json::Value, line: &str) -> ToolCall {
    let actions = actions(line);
    ToolCall {
        name: name.to_string(),
        input,
        paths: Vec::new(),
        destructive: destructive_text(line) || actions.iter().any(Action::is_destructive),
        actions,
    }
}

/// The actions of a command line, in order. Navigation-only commands
/// leave none.
pub fn actions(line: &str) -> Vec<Action> {
    actions_nested(line, 0)
}

fn actions_nested(line: &str, depth: u32) -> Vec<Action> {
    let mut out = Vec::new();
    for simple in simple_commands(line) {
        let all = words_of(&simple);
        let destructive = destructive_text(&simple);
        let words = match unwrapped(&all) {
            Runs::Program(words) => words,
            Runs::Lookup => continue,
            Runs::Unknown => {
                out.push(Action::Command {
                    program: OTHER.to_string(),
                    sub: None,
                    destructive,
                    skill: None,
                });
                continue;
            }
        };
        let Some((first, rest)) = words.split_first() else {
            continue;
        };
        let program = program_name(first);
        let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
        if SHELLS.contains(&program.as_str()) && depth < MAX_NESTING {
            let script = rest
                .iter()
                .position(|w| w.starts_with('-') && !w.starts_with("--") && w.contains('c'))
                .and_then(|at| rest.get(at + 1));
            if let Some(script) = script {
                out.extend(actions_nested(script, depth + 1));
                continue;
            }
        }
        if NAVIGATION.contains(&program.as_str()) {
            continue;
        }
        let skill = FILE_READERS
            .contains(&program.as_str())
            .then(|| rest.iter().find_map(|a| skill_of_path(a)))
            .flatten();
        let sub = if edits_in_place(&program, &rest) {
            Some(IN_PLACE.to_string())
        } else {
            subcommand(&program, &rest)
        };
        out.push(Action::Command {
            destructive,
            program,
            sub,
            skill,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_destructive_list_matches_only_what_it_names() {
        for cmd in [
            "rm -rf build",
            "rm -fr /tmp/x",
            "rm -Rf build",
            "rm -r -f build",
            "rm --recursive --force build",
            "/bin/rm -f -R build",
            "cd x && rm -rf y",
            "sudo rm -rf /var/x",
            "find . -type d -exec rm -rf {} +",
            "git push --force origin main",
            "git push -f",
            "git push --force-with-lease=main origin",
            "git push origin +main",
            "git reset --hard HEAD~1",
            "git clean -fdx",
            "git clean -d --force",
            "git branch -D old",
            "git branch --delete --force old",
            "git branch -d -f old",
            "git -C repo push --force",
            "git -c core.x=y clean -fdx",
            "git --git-dir=.git branch -D old",
            "git --no-pager -C repo push origin +main",
            "find build -name '*.o' -delete",
            "psql -c 'DROP TABLE users'",
            "mkfs.ext4 /dev/sdb1",
            "dd if=img of=/dev/sda",
            // A shell script in quotes, as codex logs every command.
            "bash -lc 'git push --force'",
            "git --frob x push --force",
            "bash -lc \"find . -name '*.o' -delete\"",
            "sudo -u deploy rm -rf /srv/x",
        ] {
            assert!(is_destructive(cmd), "{cmd}");
        }
        for cmd in [
            "rm file.txt",
            "rm -r build",
            "rm -f file.txt",
            "git push origin main",
            "git reset HEAD file",
            "git clean -n",
            "git branch -d merged",
            "git -C repo push origin main",
            "git -c push.default=current status",
            "find . -name '*.rs'",
            "ls -rf",
            "cargo build",
            "cargo test 2>&1 | tail -n 50",
            // A flag inside a quoted argument keeps its quote.
            "git commit -m \"revert the git push --force\"",
            "git commit -m \"never git push -f\"",
            "echo \"then rm -rf\"",
        ] {
            assert!(!is_destructive(cmd), "{cmd}");
        }
    }

    #[test]
    fn a_call_and_its_actions_agree_on_a_quoted_script() {
        let line = shell_words::join(["bash", "-lc", "git clean -fdx"]);
        let c = call("command_execution", serde_json::Value::Null, &line);
        assert!(c.destructive);
        assert!(c.actions.iter().any(Action::is_destructive));
    }

    #[test]
    fn a_skill_is_named_by_its_directory() {
        assert_eq!(
            skill_of_path("/home/u/.claude/skills/rust-skills/SKILL.md").as_deref(),
            Some("rust-skills")
        );
        assert_eq!(skill_of_path("/x/README.md"), None);
    }
}
