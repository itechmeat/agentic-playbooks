//! The project's own checks (a baseline `project_checks` source): the test,
//! lint, typecheck and check commands a project already defines, found
//! without running anything.
//!
//! - `package.json` scripts named `test`, `lint`, `typecheck` or `check`,
//!   run with the package manager its lockfile names (`bun.lock` or
//!   `bun.lockb`: bun, `pnpm-lock.yaml`: pnpm, `yarn.lock`: yarn, otherwise
//!   npm);
//! - a `Cargo.toml`: `cargo check` and `cargo test --no-fail-fast` (every
//!   test binary runs, so a failure in one cannot hide another's);
//! - `Makefile` and `justfile` recipes named `test` or `check` (make's own
//!   file order: `GNUmakefile`, `makefile`, `Makefile`).
//!
//! [`source_digest`] hashes what defines each command, and only that: the
//! `scripts.<kind>` entry of `package.json` with its `pre<kind>` and
//! `post<kind>` hooks, every rule naming the recipe in a Makefile or a
//! justfile, and the part of `Cargo.toml` that decides which tests run
//! (its targets, auto-discovery flags, features and workspace members; see
//! `cargo_manifest`). A definition edited between the two snapshots makes
//! the comparison unknown instead of comparing two different commands,
//! while a version or dependency bump in the same manifest does not. What a
//! definition calls is not followed: another script it runs (`npm run
//! lint && jest`), a make prerequisite's recipe or a make variable set
//! elsewhere in the file, and a workspace member's own manifest.

mod cargo_manifest;

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The check kinds a project can define, in report order.
pub const KINDS: [&str; 4] = ["test", "lint", "typecheck", "check"];

/// The `project_checks` source of a baseline check.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectChecksSpec {
    /// The kinds to run (a subset of [`KINDS`]); empty runs every kind the
    /// project defines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<String>,
}

/// One command found in the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCommand {
    /// Unique per discovery: the tool and the kind, `cargo_test`, `bun_lint`.
    pub name: String,
    /// One of [`KINDS`].
    pub kind: String,
    pub argv: Vec<String>,
    /// The file the command is defined in, relative to the root.
    pub source_file: String,
}

impl ProjectCommand {
    fn new(tool: &str, kind: &str, argv: &[&str], source_file: &str) -> Self {
        ProjectCommand {
            name: format!("{tool}_{kind}"),
            kind: kind.to_string(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            source_file: source_file.to_string(),
        }
    }

    /// Whether this command is a `cargo test` (with or without
    /// `--no-fail-fast`), the one runner whose failing tests are parsed.
    pub fn is_cargo_test(&self) -> bool {
        self.argv.first().map(String::as_str) == Some("cargo")
            && self.argv.get(1).map(String::as_str) == Some("test")
    }
}

/// The package manager a `package.json` project uses, from its lockfile.
fn node_runner(root: &Path) -> &'static str {
    if root.join("bun.lock").is_file() || root.join("bun.lockb").is_file() {
        "bun"
    } else if root.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if root.join("yarn.lock").is_file() {
        "yarn"
    } else {
        "npm"
    }
}

fn package_scripts(root: &Path, out: &mut Vec<ProjectCommand>) {
    let Ok(raw) = std::fs::read_to_string(root.join("package.json")) else {
        return;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    let Some(scripts) = doc.get("scripts").and_then(|s| s.as_object()) else {
        return;
    };
    let runner = node_runner(root);
    for kind in KINDS {
        if scripts.get(kind).is_some_and(|v| v.is_string()) {
            out.push(ProjectCommand::new(
                runner,
                kind,
                &[runner, "run", kind],
                "package.json",
            ));
        }
    }
}

fn cargo(root: &Path, out: &mut Vec<ProjectCommand>) {
    if root.join("Cargo.toml").is_file() {
        out.push(ProjectCommand::new(
            "cargo",
            "check",
            &["cargo", "check"],
            "Cargo.toml",
        ));
        out.push(ProjectCommand::new(
            "cargo",
            "test",
            &["cargo", "test", "--no-fail-fast"],
            "Cargo.toml",
        ));
    }
}

/// Whether `line` is a rule that names recipe `name` in a make or just
/// file: it does not start with whitespace or `#`, has `name` among its
/// targets before a `:` (a just recipe may be quiet, `@name`, and take
/// parameters after its name), and is not a variable assignment
/// (`name := x`, `X = a:b`).
fn names_recipe(line: &str, tool: &str, name: &str) -> bool {
    if line.starts_with(char::is_whitespace) || line.starts_with('#') {
        return false;
    }
    let Some((head, rest)) = line.split_once(':') else {
        return false;
    };
    if rest.starts_with('=') || rest.starts_with(":=") {
        return false;
    }
    let mut targets = head.split_whitespace();
    if tool == "just" {
        targets
            .next()
            .is_some_and(|t| t.strip_prefix('@').unwrap_or(t) == name)
    } else {
        !head.contains('=') && targets.any(|t| t == name)
    }
}

/// Every rule that names recipe `name` with its indented body, trailing
/// blank lines dropped. make may spread a target over several rules (a
/// prerequisite line, a target-specific variable, `test::` rules), and
/// each can change what the recipe runs.
fn recipe_text(text: &str, tool: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut rules = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if !names_recipe(line, tool, name) {
            continue;
        }
        let mut rule = vec![*line];
        rule.extend(
            lines[at + 1..]
                .iter()
                .take_while(|l| l.trim().is_empty() || l.starts_with(char::is_whitespace)),
        );
        while rule.last().is_some_and(|l| l.trim().is_empty()) {
            rule.pop();
        }
        rules.push(rule.join("\n"));
    }
    (!rules.is_empty()).then(|| rules.join("\n"))
}

fn recipes(root: &Path, tool: &str, files: &[&str], out: &mut Vec<ProjectCommand>) {
    let Some((file, text)) = files
        .iter()
        .find_map(|f| std::fs::read_to_string(root.join(f)).ok().map(|t| (*f, t)))
    else {
        return;
    };
    for kind in ["test", "check"] {
        if text.lines().any(|l| names_recipe(l, tool, kind)) {
            out.push(ProjectCommand::new(tool, kind, &[tool, kind], file));
        }
    }
}

/// The checks `root` defines, filtered to `only` (every kind when empty).
pub fn discover(root: &Path, only: &[String]) -> Vec<ProjectCommand> {
    let mut out = Vec::new();
    package_scripts(root, &mut out);
    cargo(root, &mut out);
    recipes(
        root,
        "make",
        &["GNUmakefile", "makefile", "Makefile"],
        &mut out,
    );
    recipes(
        root,
        "just",
        &["justfile", "Justfile", ".justfile"],
        &mut out,
    );
    out.retain(|c| only.is_empty() || only.contains(&c.kind));
    out
}

/// What defines `cmd` in its source file (see [`source_digest`]). `None`
/// when the file or the definition is gone.
fn definition(root: &Path, cmd: &ProjectCommand) -> Option<String> {
    let tool = cmd.argv.first().map(String::as_str).unwrap_or("");
    let text = std::fs::read_to_string(root.join(&cmd.source_file)).ok()?;
    if tool == "cargo" {
        return Some(cargo_manifest::test_selection(&text));
    }
    if cmd.source_file == "package.json" {
        return script_with_hooks(&text, &cmd.kind);
    }
    recipe_text(&text, tool, &cmd.kind)
}

/// `scripts.<kind>` of a `package.json` with the `pre<kind>` and
/// `post<kind>` scripts npm, pnpm and yarn run around it.
fn script_with_hooks(text: &str, kind: &str) -> Option<String> {
    let doc: serde_json::Value = serde_json::from_str(text).ok()?;
    let scripts = doc.get("scripts")?;
    let mut out = scripts.get(kind)?.as_str()?.to_string();
    for hook in [format!("pre{kind}"), format!("post{kind}")] {
        if let Some(body) = scripts.get(&hook) {
            out.push_str(&format!("\n{hook}: {body}"));
        }
    }
    Some(out)
}

/// The digest of what defines each of `commands` (see [`definition`]), in
/// command order. A definition that is gone hashes as missing, so it still
/// changes the digest when it comes back.
pub fn source_digest(root: &Path, commands: &[ProjectCommand]) -> String {
    let mut acc = Vec::new();
    for cmd in commands {
        acc.extend_from_slice(cmd.name.as_bytes());
        acc.push(0);
        match definition(root, cmd) {
            Some(d) => acc.extend_from_slice(crate::content::sha256_hex(d.as_bytes()).as_bytes()),
            None => acc.extend_from_slice(b"missing"),
        }
        acc.push(0);
    }
    crate::content::sha256_hex(&acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(cmds: &[ProjectCommand]) -> Vec<String> {
        cmds.iter().map(|c| c.argv.join(" ")).collect()
    }

    #[test]
    fn a_bun_project_runs_its_scripts_with_bun() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"vitest","lint":"eslint .","build":"vite build"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("bun.lock"), "").unwrap();
        assert_eq!(
            names(&discover(dir.path(), &[])),
            ["bun run test", "bun run lint"]
        );
        std::fs::remove_file(dir.path().join("bun.lock")).unwrap();
        assert_eq!(
            names(&discover(dir.path(), &["lint".into()])),
            ["npm run lint"]
        );
    }

    #[test]
    fn a_cargo_project_checks_and_tests() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let cmds = discover(dir.path(), &["test".into()]);
        assert_eq!(names(&cmds), ["cargo test --no-fail-fast"]);
        assert!(cmds[0].is_cargo_test());
        assert_eq!(cmds[0].name, "cargo_test");
    }

    #[test]
    fn make_and_just_recipes_named_test_or_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "build:\n\tcc x.c\ntest: build\n\t./x\ntesting:\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("justfile"), "check filter='':\n  echo\n").unwrap();
        assert_eq!(
            names(&discover(dir.path(), &[])),
            ["make test", "just check"]
        );
    }

    #[test]
    fn recipes_in_every_shape_make_and_just_write_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Makefile"), "build test\t: deps\n\t./x\n").unwrap();
        std::fs::write(dir.path().join("justfile"), "@check:\n  echo\n").unwrap();
        assert_eq!(
            names(&discover(dir.path(), &[])),
            ["make test", "just check"]
        );
        // Variables are not recipes.
        std::fs::write(dir.path().join("Makefile"), "test = a:b\n").unwrap();
        std::fs::write(dir.path().join("justfile"), "check := \"x\"\n").unwrap();
        assert!(discover(dir.path(), &[]).is_empty());
    }

    #[test]
    fn make_reads_the_file_gnu_make_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Makefile"), "test:\n\tfalse\n").unwrap();
        std::fs::write(dir.path().join("GNUmakefile"), "check:\n\ttrue\n").unwrap();
        let cmds = discover(dir.path(), &[]);
        assert_eq!(names(&cmds), ["make check"]);
        assert_eq!(cmds[0].source_file, "GNUmakefile");
    }

    #[test]
    fn the_source_digest_follows_the_definition_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"a"}}"#,
        )
        .unwrap();
        let cmds = discover(dir.path(), &[]);
        let before = source_digest(dir.path(), &cmds);
        assert_eq!(before, source_digest(dir.path(), &cmds));
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"b"}}"#,
        )
        .unwrap();
        assert_ne!(before, source_digest(dir.path(), &cmds));
    }

    fn digest_changes(file: &str, before: &str, after: &str) -> bool {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(file), before).unwrap();
        let cmds = discover(dir.path(), &["test".into()]);
        assert_eq!(cmds.len(), 1, "{file}");
        let digest = source_digest(dir.path(), &cmds);
        std::fs::write(dir.path().join(file), after).unwrap();
        digest != source_digest(dir.path(), &cmds)
    }

    #[test]
    fn an_npm_hook_around_the_script_is_part_of_its_definition() {
        let plain = r#"{"scripts":{"test":"jest"}}"#;
        assert!(digest_changes(
            "package.json",
            plain,
            r#"{"scripts":{"test":"jest","pretest":"rm -rf tests/slow"}}"#
        ));
        assert!(digest_changes(
            "package.json",
            r#"{"scripts":{"test":"jest","posttest":"a"}}"#,
            r#"{"scripts":{"test":"jest","posttest":"b"}}"#
        ));
        assert!(!digest_changes(
            "package.json",
            plain,
            r#"{"scripts":{"test":"jest","prelint":"x"}}"#
        ));
    }

    #[test]
    fn a_cargo_manifest_change_to_test_selection_changes_the_digest() {
        let manifest = "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\n";
        for changed in [
            "[package]\nname = \"x\"\nautotests = false\n\n[dependencies]\nserde = \"1\"\n",
            "[package]\nname = \"x\"\n\n[[test]]\nname = \"api\"\nharness = false\n\n[dependencies]\nserde = \"1\"\n",
            "[package]\nname = \"x\"\n\n[features]\ndefault = [\"slow\"]\n\n[dependencies]\nserde = \"1\"\n",
        ] {
            assert!(digest_changes("Cargo.toml", manifest, changed), "{changed}");
        }
        for bumped in [
            "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"2\"\n",
            "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\n\n[target.'cfg(unix)'.dev-dependencies]\nlibc = \"0.2\"\n\n[dependencies.tokio]\nversion = \"1\"\n",
            "[package]\nname = \"x\"\nversion = \"0.2.0\"\ndescription = \"y\"\n\n[dependencies]\nserde = \"1\"\n",
            "[package]\nname = \"x\" # the crate\n\n[dependencies]\nserde = \"1\"\n",
        ] {
            assert!(!digest_changes("Cargo.toml", manifest, bumped), "{bumped}");
        }
    }

    #[test]
    fn a_cargo_package_version_bump_keeps_the_digest_and_autotests_changes_it() {
        let manifest = "[package]\nname = \"x\"\nversion = \"1.0.0\"\nautotests = true\n";
        assert!(!digest_changes(
            "Cargo.toml",
            manifest,
            &manifest.replace("1.0.0", "1.1.0")
        ));
        assert!(digest_changes(
            "Cargo.toml",
            manifest,
            &manifest.replace("autotests = true", "autotests = false")
        ));
    }

    #[test]
    fn multi_line_values_do_not_break_the_cargo_manifest_reading() {
        // A line inside a multi-line string or array is never a header.
        let manifest = "[package]\nname = \"x\"\ndescription = \"\"\"\n[[test]]\nname = \"a\"\n\"\"\"\n\n[workspace]\nmembers = [\n  \"a\",\n  \"b\", # [dependencies]\n]\n\n[features]\n\"x#y\" = []\n";
        assert!(!digest_changes(
            "Cargo.toml",
            manifest,
            &manifest.replace("name = \"a\"", "name = \"b\"")
        ));
        assert!(digest_changes(
            "Cargo.toml",
            manifest,
            &manifest.replace("\"b\",", "\"c\",")
        ));
        assert!(digest_changes(
            "Cargo.toml",
            manifest,
            &manifest.replace("x#y", "x#z")
        ));
        assert!(!digest_changes(
            "Cargo.toml",
            manifest,
            &manifest.replace("# [dependencies]", "# members")
        ));
    }

    #[test]
    fn a_cargo_manifest_that_is_gone_reads_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let cmds = discover(dir.path(), &["test".into()]);
        let before = source_digest(dir.path(), &cmds);
        std::fs::remove_file(dir.path().join("Cargo.toml")).unwrap();
        assert_ne!(before, source_digest(dir.path(), &cmds));
        assert_eq!(definition(dir.path(), &cmds[0]), None);
    }

    #[test]
    fn every_make_rule_naming_the_target_is_part_of_its_recipe() {
        // The prerequisite line comes first; the command is in a later rule.
        let split = "test: build\nbuild:\n\tcc\ntest:\n\t./run\n";
        assert!(digest_changes(
            "Makefile",
            split,
            &split.replace("./run", "./run --all")
        ));
        assert!(digest_changes(
            "Makefile",
            "test: FLAGS=-v\ntest:\n\t./run $(FLAGS)\n",
            "test: FLAGS=-q\ntest:\n\t./run $(FLAGS)\n"
        ));
        assert!(digest_changes(
            "Makefile",
            "test::\n\t./a\nbuild:\n\tcc\ntest::\n\t./b\n",
            "test::\n\t./a\nbuild:\n\tcc\ntest::\n\t./c\n"
        ));
        // Blank lines after the recipe are not part of it.
        assert!(!digest_changes(
            "Makefile",
            "test:\n\t./run\nbuild:\n\tcc\n",
            "test:\n\t./run\n\n\n\nbuild:\n\tcc\n"
        ));
    }

    #[test]
    fn a_dependency_bump_leaves_the_source_digest_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"a"},"dependencies":{"x":"1.0.0"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[dependencies]\nx = \"1\"\n").unwrap();
        std::fs::write(dir.path().join("Makefile"), "test:\n\t./t\nbuild:\n\tcc\n").unwrap();
        let cmds = discover(dir.path(), &[]);
        let before = source_digest(dir.path(), &cmds);
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"a"},"dependencies":{"x":"2.0.0"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[dependencies]\nx = \"2\"\n").unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "test:\n\t./t\nbuild:\n\tcc -O2\n",
        )
        .unwrap();
        assert_eq!(before, source_digest(dir.path(), &cmds));
        std::fs::write(dir.path().join("Makefile"), "test:\n\t./t --all\n").unwrap();
        assert_ne!(before, source_digest(dir.path(), &cmds));
    }
}
