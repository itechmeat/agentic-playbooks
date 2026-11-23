//! A binary compiled from the published crate `agentic-playbooks` came from
//! `cargo install`. The kind of install is a property of the binary, fixed
//! when it was compiled from the package name cargo built: no file on disk
//! decides it (an installer receipt and cargo's own install record can both
//! sit next to a binary neither of them wrote).

use axoupdater::Version;

/// The name the CLI package is published under on crates.io.
pub(crate) const PUBLISHED_PACKAGE: &str = "agentic-playbooks";

/// Whether a binary compiled as `package` came from `cargo install`.
pub(crate) fn built_for_cargo_install(package: &str) -> bool {
    package == PUBLISHED_PACKAGE
}

/// What `apb self-update` says on a cargo install of `running`.
pub(crate) fn guidance(running: &Version) -> String {
    let mut text = format!(
        "apb was installed with cargo. Update it with: cargo install --locked {PUBLISHED_PACKAGE}"
    );
    if !running.pre.is_empty() {
        text.push_str(
            " --version <the version you want> (pre-releases are installed only by version)",
        );
    }
    text.push_str(". apb self-update updates only installs made by the apb installer.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_published_package_is_a_cargo_build() {
        assert!(built_for_cargo_install("agentic-playbooks"));
        assert!(!built_for_cargo_install("apb"));
        assert!(!built_for_cargo_install("agentic-playbooks-core"));
    }

    #[test]
    fn a_stable_build_names_the_cargo_command() {
        assert_eq!(
            guidance(&Version::parse("1.0.0").unwrap()),
            "apb was installed with cargo. Update it with: cargo install --locked agentic-playbooks. \
             apb self-update updates only installs made by the apb installer."
        );
    }

    #[test]
    fn a_prerelease_build_says_to_name_the_version() {
        assert_eq!(
            guidance(&Version::parse("1.0.0-rc.1").unwrap()),
            "apb was installed with cargo. Update it with: cargo install --locked agentic-playbooks \
             --version <the version you want> (pre-releases are installed only by version). \
             apb self-update updates only installs made by the apb installer."
        );
    }
}
