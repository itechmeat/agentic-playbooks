//! A node's `isolation`: the level, the network and the extra paths an
//! `agent_task` declares, and the pure contract that decides what this
//! machine can enforce for it ([`contract`]). The paths an author may name
//! and the credential locations nothing may bind live in [`paths`].
//!
//! ```yaml
//! isolation: full                 # shorthand: { level: full }
//! isolation:
//!   level: full                   # full | best_effort | none
//!   network: open                 # open | none; omitted = open
//!   read_only: [~/.rustup]
//!   writable: [~/.cargo/registry]
//! ```

pub mod contract;
pub mod paths;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How strictly a node is isolated. `full` refuses to run what this machine
/// cannot enforce, `best_effort` runs and lists what it could not, `none`
/// (the default) runs in the shared tree exactly as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Full,
    BestEffort,
    #[default]
    None,
}

impl Level {
    /// The wire name (`full`, `best_effort`, `none`).
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Full => "full",
            Level::BestEffort => "best_effort",
            Level::None => "none",
        }
    }
}

/// The network an isolated agent gets: the host's (`open`) or loopback
/// only (`none`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    #[default]
    Open,
    None,
}

impl Network {
    /// The wire name (`open`, `none`).
    pub fn as_str(self) -> &'static str {
        match self {
            Network::Open => "open",
            Network::None => "none",
        }
    }
}

/// A node's declared isolation. Parsed from the plain level string or the
/// long map; a spec that holds only a level serializes back as the string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IsolationSpec {
    pub level: Level,
    /// `None`: left implicit, which means [`Network::Open`] (V112 warns
    /// under `full`).
    pub network: Option<Network>,
    /// Extra paths bound read-only (`~/` or absolute).
    pub read_only: Vec<String>,
    /// Extra paths bound read-write (`~/` or absolute).
    pub writable: Vec<String>,
}

impl IsolationSpec {
    /// A spec of `level` with nothing else declared.
    pub fn of(level: Level) -> Self {
        IsolationSpec {
            level,
            ..IsolationSpec::default()
        }
    }

    /// Whether the node runs isolated at all (`full` or `best_effort`).
    pub fn is_isolated(&self) -> bool {
        self.level != Level::None
    }

    /// The network in force: the declared one, `open` when implicit.
    pub fn network(&self) -> Network {
        self.network.unwrap_or_default()
    }

    /// Whether anything beyond the level is declared.
    fn has_extras(&self) -> bool {
        self.network.is_some() || !self.read_only.is_empty() || !self.writable.is_empty()
    }
}

/// The `isolation:` section of the global config.
///
/// ```yaml
/// isolation:
///   bwrap: /usr/local/bin/bwrap   # default: /usr/bin/bwrap, then PATH
///   read_only: [~/.rustup, /opt/sdk]
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct IsolationSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bwrap: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub read_only: Vec<String>,
}

impl IsolationSettings {
    /// `bwrap` must be absolute; every `read_only` path passes the same
    /// check as a node's (V114).
    pub fn validate(&self) -> Result<(), String> {
        if let Some(b) = &self.bwrap
            && !std::path::Path::new(b).is_absolute()
        {
            return Err(format!("isolation.bwrap `{b}` must be an absolute path"));
        }
        for p in &self.read_only {
            paths::check_extra(p).map_err(|why| format!("isolation.read_only path `{p}` {why}"))?;
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LongForm {
    #[serde(default)]
    level: Level,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    network: Option<Network>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    read_only: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    writable: Vec<String>,
}

/// Reads the two YAML forms of `isolation`, a level name or the long map,
/// so a wrong level or an unknown key keeps serde's own message.
struct SpecVisitor;

impl<'de> serde::de::Visitor<'de> for SpecVisitor {
    type Value = IsolationSpec;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an isolation level or a map with `level`, `network`, `read_only`, `writable`")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<IsolationSpec, E> {
        use serde::de::IntoDeserializer;
        Level::deserialize(v.into_deserializer()).map(IsolationSpec::of)
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<IsolationSpec, A::Error> {
        let l = LongForm::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
        Ok(IsolationSpec {
            level: l.level,
            network: l.network,
            read_only: l.read_only,
            writable: l.writable,
        })
    }
}

impl<'de> Deserialize<'de> for IsolationSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(SpecVisitor)
    }
}

impl Serialize for IsolationSpec {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if !self.has_extras() {
            return self.level.serialize(s);
        }
        LongForm {
            level: self.level,
            network: self.network,
            read_only: self.read_only.clone(),
            writable: self.writable.clone(),
        }
        .serialize(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_settings_are_checked() {
        let ok: IsolationSettings =
            yaml_serde::from_str("{ bwrap: /usr/bin/bwrap, read_only: [~/.rustup] }").unwrap();
        assert_eq!(ok.validate(), Ok(()));
        let rel = IsolationSettings {
            bwrap: Some("bwrap".into()),
            ..IsolationSettings::default()
        };
        assert!(rel.validate().is_err());
        let cred = IsolationSettings {
            read_only: vec!["~/.ssh".into()],
            ..IsolationSettings::default()
        };
        assert!(cred.validate().is_err());
    }

    #[test]
    fn the_shorthand_round_trips_as_a_string() {
        let s: IsolationSpec = yaml_serde::from_str("full").unwrap();
        assert_eq!(s, IsolationSpec::of(Level::Full));
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"full\"");
        let long: IsolationSpec = yaml_serde::from_str("{ level: best_effort }").unwrap();
        assert_eq!(serde_json::to_string(&long).unwrap(), "\"best_effort\"");
    }

    #[test]
    fn the_long_form_keeps_its_extras_and_refuses_unknown_keys() {
        let s: IsolationSpec =
            yaml_serde::from_str("{ level: full, network: none, read_only: [~/.rustup] }").unwrap();
        assert_eq!(s.network(), Network::None);
        assert_eq!(s.read_only, vec!["~/.rustup".to_string()]);
        let back: IsolationSpec =
            serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let typo = yaml_serde::from_str::<IsolationSpec>("{ level: full, net: none }")
            .unwrap_err()
            .to_string();
        assert!(typo.contains("unknown field `net`"), "{typo}");
        let level = yaml_serde::from_str::<IsolationSpec>("strict")
            .unwrap_err()
            .to_string();
        assert!(level.contains("unknown variant `strict`"), "{level}");
        let scalar = yaml_serde::from_str::<IsolationSpec>("{ level: full, read_only: ~/.x }")
            .unwrap_err()
            .to_string();
        assert!(!scalar.contains("untagged"), "{scalar}");
    }
}
