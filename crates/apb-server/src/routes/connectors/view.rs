//! The public, secret-free view of a connector: what the dashboard is
//! allowed to render about a manifest, whether installed or only embedded.

use apb_core::connector::store;

/// The installation-independent half of the connector detail response: the
/// parsed manifest plus the storefront page. It comes off disk for an
/// installed connector and straight out of the binary for an embedded official
/// one that is merely not installed, which is why the two are resolved
/// together here rather than at each use site.
pub(crate) struct ConnectorPublic {
    pub(crate) doc: apb_core::connector::def::ConnectorDoc,
    pub(crate) meta: store::PublicMeta,
    pub(crate) body_md: String,
}

/// The installation state of a connector, resolved alongside its public half.
/// `Invalid` is kept distinct from `NotInstalled` so a broken installed
/// connector (its `store::load` fails with a non-`NotFound` error) surfaces as
/// installed with trust `invalid`, rather than masquerading as a
/// not-yet-connected embedded connector. That keeps the detail endpoint in step
/// with the list endpoint, which already reports `invalid` for the very same
/// connector.
///
/// The loaded connector is boxed to keep the enum small (the parseable variant
/// would otherwise dwarf the two unit variants).
pub(crate) enum InstallState {
    /// Installed and parseable; trust comes from the tree digest.
    Installed(Box<store::LoadedConnector>),
    /// Not installed; the public half is served from the embedded official
    /// manifest.
    NotInstalled,
    /// Installed but broken (unparseable connector.yaml, digest or containment
    /// error): reported as installed with trust `invalid`.
    Invalid,
}

/// The public half of an embedded official connector, or `None` when `name` is
/// not embedded. (Its embedded manifest failing to parse cannot happen for the
/// shipped set, but is handled as `None` all the same.)
pub(crate) fn embedded_public(name: &str) -> Option<ConnectorPublic> {
    let embedded = apb_core::connector::official::get(name)?;
    Some(ConnectorPublic {
        doc: embedded.doc()?,
        meta: embedded.meta(),
        body_md: embedded.public_body(),
    })
}

/// The manifest and storefront page of `name`, plus how it is installed. `None`
/// for a name that is neither installed nor embedded - the only case the detail
/// endpoint answers with a 404.
///
/// Not being installed is a state, not an error: every official connector's
/// manifest is baked into this binary and none of it is private, so the public
/// half is served either way and only the on-disk `LoadedConnector` is absent.
///
/// A connector whose `store::load` fails with a non-`NotFound` error IS
/// installed, just broken (unparseable connector.yaml, a digest or containment
/// error). It must not be swallowed the way `NotFound` is and reported as a
/// connectable embedded connector: that would disagree with the list endpoint,
/// which already reports the same connector's trust as `invalid`. It resolves
/// to `InstallState::Invalid` instead, with the storefront half filled from the
/// embedded manifest when the connector is official, so the two views agree.
pub(crate) fn connector_public(name: &str) -> Option<(ConnectorPublic, InstallState)> {
    match store::load(name) {
        Ok(loaded) => {
            let public = ConnectorPublic {
                doc: loaded.doc.clone(),
                meta: store::public_meta(&loaded.dir),
                body_md: store::public_body(&loaded.dir),
            };
            Some((public, InstallState::Installed(Box::new(loaded))))
        }
        // Not installed: nothing on disk. Fall back to the embedded official
        // manifest so the storefront can show what the connector does before it
        // is connected. A name that is neither installed nor embedded is the
        // one case that 404s (the `?` below).
        Err(apb_core::connector::ConnectorError::NotFound(_)) => {
            Some((embedded_public(name)?, InstallState::NotInstalled))
        }
        // Installed but broken: report installed + `invalid`, mirroring the
        // list endpoint. Serve the storefront half from the embedded manifest
        // when this is an official connector, else a minimal one so the
        // endpoint still answers 200 with an honest invalid state.
        Err(_) => {
            let public = embedded_public(name).unwrap_or_else(|| ConnectorPublic {
                doc: apb_core::connector::def::ConnectorDoc {
                    name: name.to_string(),
                    version: String::new(),
                    healthcheck: None,
                    auth: None,
                    error_when: None,
                    webhook: None,
                    account_fields: Vec::new(),
                    functions: Vec::new(),
                },
                meta: store::public_meta_from_str("", name),
                body_md: String::new(),
            });
            Some((public, InstallState::Invalid))
        }
    }
}
