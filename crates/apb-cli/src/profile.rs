use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Subcommand};

use crate::manage::offer_onboarding_if_tty;
use crate::util::print_json;

#[derive(Subcommand)]
pub(crate) enum ProfileAction {
    /// List profiles (project and global scope)
    List,
    /// Show a profile's content
    Show {
        name: String,
        #[arg(long, default_value = "project")]
        scope: String,
    },
    /// Copy a profile between scopes (source stays)
    Move {
        name: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    /// Delete a profile (blocked by references unless --force)
    Delete {
        name: String,
        #[arg(long, default_value = "project")]
        scope: String,
        #[arg(long)]
        force: bool,
    },
    /// Create or update a profile (auto-approves its bundle)
    Write(ProfileWriteArgs),
    /// Edit a profile's profile.yaml and SOUL.md in $EDITOR (CAS on save)
    Edit {
        name: String,
        #[arg(long, default_value = "project")]
        scope: String,
    },
}

#[derive(Args)]
pub(crate) struct ProfileWriteArgs {
    name: String,
    #[arg(long, default_value = "project")]
    scope: String,
    #[arg(long)]
    agent: String,
    #[arg(long)]
    model: String,
    /// Fallback executor as agent:model (repeatable, ordered)
    #[arg(long = "fallback", value_name = "AGENT:MODEL")]
    fallbacks: Vec<String>,
    /// Skill name (repeatable)
    #[arg(long = "skill", value_name = "NAME")]
    skills: Vec<String>,
    /// File whose contents become SOUL.md (empty if omitted)
    #[arg(long)]
    soul: Option<PathBuf>,
    #[arg(long, default_value = "")]
    description: String,
    /// SOUL requirement: any | native_required
    #[arg(long = "soul-requirement", default_value = "any")]
    soul_requirement: String,
    /// Update an existing profile: digest it must currently match (CAS)
    #[arg(long)]
    expected_digest: Option<String>,
}

pub(crate) fn profile_cmd(root: &Path, action: ProfileAction) -> ExitCode {
    use apb_mcp::profile_tools;
    let res = match action {
        ProfileAction::List => profile_tools::profile_list(root),
        ProfileAction::Show { name, scope } => profile_tools::profile_get(root, &name, &scope),
        ProfileAction::Move { name, from, to } => {
            profile_tools::profile_move(root, &name, &from, &to)
        }
        ProfileAction::Delete { name, scope, force } => {
            profile_tools::profile_delete(root, &name, &scope, force)
        }
        ProfileAction::Write(args) => profile_write_cmd(root, args),
        ProfileAction::Edit { name, scope } => return profile_edit_cmd(root, &name, &scope),
    };
    match res {
        Ok(v) => {
            print_json(&v);
            // Working with profiles is also a good point to offer the
            // subscriptions survey (interactively, if onboarding hasn't run).
            offer_onboarding_if_tty();
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("profile error: {e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn parse_fallback(s: &str) -> Result<(String, String), String> {
    let (a, m) = s
        .split_once(':')
        .ok_or_else(|| format!("fallback `{s}` must be agent:model"))?;
    if a.is_empty() || m.is_empty() {
        return Err(format!("fallback `{s}` must be agent:model"));
    }
    Ok((a.to_string(), m.to_string()))
}

/// Assembles a ProfileWrite from the parsed CLI flags and calls the shared
/// profile_write logic (the same one the MCP tool uses: validation, CAS lock,
/// bundle auto-approve). Returns Result for the single match in profile_cmd.
pub(crate) fn profile_write_cmd(
    root: &Path,
    args: ProfileWriteArgs,
) -> Result<serde_json::Value, apb_mcp::tools::ToolError> {
    use apb_mcp::profile_tools::{self, ExecutorInput, ProfileWrite};
    let soul_requirement = profile_tools::parse_soul_requirement(Some(&args.soul_requirement))
        .map_err(apb_mcp::tools::ToolError::Engine)?;
    let mut fbs = Vec::new();
    for f in &args.fallbacks {
        fbs.push(parse_fallback(f).map_err(apb_mcp::tools::ToolError::Engine)?);
    }
    let soul_md = match &args.soul {
        Some(p) => std::fs::read_to_string(p)
            .map_err(|e| apb_mcp::tools::ToolError::Engine(format!("read soul file: {e}")))?,
        None => String::new(),
    };
    profile_tools::profile_write(
        root,
        ProfileWrite {
            name: args.name,
            scope: args.scope,
            description: args.description,
            soul_md,
            skills: profile_tools::skill_refs(&args.skills),
            executor: ExecutorInput {
                agent: args.agent,
                model: args.model,
                fallbacks: fbs,
            },
            expected_digest: args.expected_digest,
            soul_requirement,
        },
    )
}

/// `apb profile edit`: opens profile.yaml and SOUL.md in $EDITOR, then saves
/// through the same profile_write logic with CAS against the digest taken
/// BEFORE editing. A concurrent change (digest drifted) is a reported
/// conflict, not an overwrite.
pub(crate) fn profile_edit_cmd(root: &Path, name: &str, scope: &str) -> ExitCode {
    use apb_mcp::profile_tools::{self, ExecutorInput};
    // Validate the name BEFORE building paths (consistent with
    // profile_get/write; an invalid name should not even attempt to read the
    // directory).
    if let Err(e) = apb_core::profile::validate_profile_name(name) {
        eprintln!("profile error: {e}");
        return ExitCode::from(2);
    }
    let scope_enum = match scope {
        "project" => apb_core::profile::ProfileScope::Project,
        "global" => apb_core::profile::ProfileScope::Global,
        other => {
            eprintln!("profile error: unknown scope `{other}`");
            return ExitCode::from(2);
        }
    };
    let dir = match scope_enum {
        apb_core::profile::ProfileScope::Global => match apb_core::config::config_dir() {
            Some(d) => d.join("profiles").join(name),
            None => {
                eprintln!("profile error: no config dir for global scope");
                return ExitCode::from(2);
            }
        },
        _ => root.join(".apb/profiles").join(name),
    };
    let yaml_path = dir.join("profile.yaml");
    let cur_yaml = match std::fs::read_to_string(&yaml_path) {
        Ok(y) => y,
        Err(e) => {
            eprintln!("profile error: cannot read {}: {e}", yaml_path.display());
            return ExitCode::from(2);
        }
    };
    // A missing SOUL.md is normal (empty role); any other read error is NOT
    // swallowed into emptiness, otherwise the CAS digest would be computed
    // against a wrong "empty" SOUL and could overwrite a real SOUL on write.
    let soul_path = dir.join("SOUL.md");
    let cur_soul = match std::fs::read_to_string(&soul_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("profile error: cannot read {}: {e}", soul_path.display());
            return ExitCode::from(2);
        }
    };
    // Digest BEFORE editing is the basis for CAS: we save exactly the version
    // we saw.
    let digest_before = apb_core::profile::profile_digest(&cur_yaml, &cur_soul);

    // We edit copies in temp, not the profile directory itself (a profile is
    // published atomically through profile_write; a direct edit would bypass
    // CAS and bundle-approve). The private edit directory is created
    // EXCLUSIVELY (create, not create_all): a path or symlink with this name
    // planted in advance in the shared /tmp is rejected (AlreadyExists), so
    // an attacker cannot redirect the write into another directory. The name
    // is unpredictable (pid + nanoseconds), mode 0700.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "apb-profile-edit-{}-{unique}-{name}",
        std::process::id()
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    if let Err(e) = builder.create(&tmp) {
        eprintln!("profile error: cannot create private temp edit dir: {e}");
        return ExitCode::from(2);
    }
    let tmp_yaml = tmp.join("profile.yaml");
    let tmp_soul = tmp.join("SOUL.md");
    // A temp write error is NOT swallowed (otherwise the editor would open
    // an empty file and we'd overwrite the profile with empty content).
    if let Err(e) =
        std::fs::write(&tmp_yaml, &cur_yaml).and_then(|_| std::fs::write(&tmp_soul, &cur_soul))
    {
        eprintln!("profile error: cannot stage temp files: {e}");
        let _ = std::fs::remove_dir_all(&tmp);
        return ExitCode::from(2);
    }

    // $VISUAL takes priority over $EDITOR (POSIX convention). The value is a
    // command line: we split it into program + arguments so that
    // `EDITOR="code --wait"`, `vim -f`, etc. launch correctly instead of
    // being treated as a single executable name.
    let editor_cmd = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".to_string());
    let parts: Vec<String> = editor_cmd
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let (prog, extra) = parts.split_first().expect("editor command non-empty");
    let status = std::process::Command::new(prog)
        .args(extra)
        .arg(&tmp_yaml)
        .arg(&tmp_soul)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("profile error: editor exited with {:?}", s.code());
            let _ = std::fs::remove_dir_all(&tmp);
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("profile error: cannot launch editor `{editor_cmd}`: {e}");
            let _ = std::fs::remove_dir_all(&tmp);
            return ExitCode::from(2);
        }
    }

    // A read error on the result is NOT turned into empty content; the temp
    // files are left for recovery.
    let (new_yaml, new_soul) = match (
        std::fs::read_to_string(&tmp_yaml),
        std::fs::read_to_string(&tmp_soul),
    ) {
        (Ok(y), Ok(s)) => (y, s),
        _ => {
            eprintln!(
                "profile error: cannot read edited files; left for recovery in {}",
                tmp.display()
            );
            return ExitCode::from(2);
        }
    };
    // We do NOT remove staging here: we keep the user's edits on disk until a
    // successful publish through profile_write. Any validation/write failure
    // below leaves the edited files for recovery.
    let doc = match apb_core::profile::ProfileDoc::from_yaml(&new_yaml) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "profile error: edited profile.yaml is invalid: {e}; edited files kept in {}",
                tmp.display()
            );
            return ExitCode::from(2);
        }
    };
    // Renaming via edit is not supported: changing `name:` would lead to a
    // write under a new name while doing CAS against the old digest, i.e. a
    // confusing conflict, and the original profile would remain untouched.
    // We reject it explicitly (use profile_move to change name/scope).
    if doc.name != name {
        eprintln!(
            "profile error: renaming via edit is not supported (name `{}` != `{name}`); use profile move; edited files kept in {}",
            doc.name,
            tmp.display()
        );
        return ExitCode::from(2);
    }
    let res = profile_tools::profile_write(
        root,
        profile_tools::ProfileWrite {
            name: doc.name.clone(),
            scope: scope.to_string(),
            description: doc.description.clone(),
            soul_md: new_soul,
            skills: doc.skills.clone(),
            executor: ExecutorInput {
                agent: doc.executor.agent.clone(),
                model: doc.executor.model.clone(),
                fallbacks: doc
                    .executor
                    .fallbacks
                    .iter()
                    .map(|f| (f.agent.clone(), f.model.clone()))
                    .collect(),
            },
            expected_digest: Some(digest_before),
            soul_requirement: doc.soul,
        },
    );
    match res {
        Ok(v) => {
            // The publish succeeded - only now do we remove staging.
            let _ = std::fs::remove_dir_all(&tmp);
            print_json(&v);
            // Like other `apb profile *` commands, offer the onboarding survey interactively.
            offer_onboarding_if_tty();
            ExitCode::SUCCESS
        }
        Err(e) => {
            // The write failed (including a CAS conflict): we keep staging so
            // the user's edits are not lost.
            eprintln!("profile error: {e}; edited files kept in {}", tmp.display());
            ExitCode::from(2)
        }
    }
}
