//! claude's `~/.claude.json` as the sandbox home gets it. The operator's
//! file holds far more than a headless start needs: every user-scope MCP
//! server with its `env` and `headers` (tokens of other services), the path
//! and history of every project, repository paths and caches. The sandbox
//! copy keeps only the keys below, so none of that reaches an isolated
//! agent.

use serde_json::{Map, Value};

/// The top-level keys the copy keeps: onboarding and permission-mode
/// flags, the anonymous install id and the ids of the account the login
/// belongs to.
const KEPT: [&str; 8] = [
    "hasCompletedOnboarding",
    "lastOnboardingVersion",
    "bypassPermissionsModeAccepted",
    "hasAcknowledgedCostThreshold",
    "userID",
    "installMethod",
    "autoUpdates",
    "oauthAccount",
];

/// The `oauthAccount` keys the copy keeps: the account's and the
/// organization's ids, never the name, the email or anything added later.
const ACCOUNT_KEPT: [&str; 2] = ["accountUuid", "organizationUuid"];

/// The sandbox copy of an operator's `~/.claude.json`: the [`KEPT`] keys,
/// `oauthAccount` reduced to [`ACCOUNT_KEPT`]. Anything that does not parse
/// becomes an empty object.
pub fn minimal_state(bytes: &[u8]) -> Vec<u8> {
    let Ok(Value::Object(full)) = serde_json::from_slice::<Value>(bytes) else {
        return b"{}".to_vec();
    };
    let mut out = Map::new();
    for key in KEPT {
        let Some(v) = full.get(key) else { continue };
        let v = match (key, v) {
            ("oauthAccount", Value::Object(account)) => Value::Object(
                ACCOUNT_KEPT
                    .iter()
                    .filter_map(|k| {
                        let v = account.get(*k).filter(|v| v.is_string())?;
                        Some((k.to_string(), v.clone()))
                    })
                    .collect(),
            ),
            (_, v) if v.is_object() || v.is_array() => continue,
            (_, v) => v.clone(),
        };
        out.insert(key.to_string(), v);
    }
    serde_json::to_vec_pretty(&Value::Object(out)).unwrap_or_else(|_| b"{}".to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "fixture-token-0123456789";

    #[test]
    fn the_copy_keeps_the_start_flags_and_drops_every_other_service() {
        let full = format!(
            r#"{{
              "hasCompletedOnboarding": true,
              "userID": "u1",
              "oauthAccount": {{ "accountUuid": "a1", "emailAddress": "me@example.invalid", "newSecret": "{TOKEN}", "extra": {{ "k": "{TOKEN}" }} }},
              "mcpServers": {{ "gh": {{ "command": "x", "env": {{ "GITHUB_TOKEN": "{TOKEN}" }} }},
                              "web": {{ "url": "https://x.invalid", "headers": {{ "Authorization": "Bearer {TOKEN}" }} }} }},
              "projects": {{ "/home/u/p": {{ "mcpServers": {{ "a": {{ "env": {{ "T": "{TOKEN}" }} }} }} }} }},
              "githubRepoPaths": {{ "o/r": ["/home/u/p"] }},
              "env": {{ "X": "{TOKEN}" }}
            }}"#
        );
        let copy = String::from_utf8(minimal_state(full.as_bytes())).unwrap();
        assert!(!copy.contains(TOKEN), "a fixture token reached the copy");
        for gone in [
            "mcpServers",
            "projects",
            "githubRepoPaths",
            "\"env\"",
            "/home/u/p",
        ] {
            assert!(!copy.contains(gone), "{gone}");
        }
        let v: Value = serde_json::from_str(&copy).unwrap();
        assert_eq!(v["hasCompletedOnboarding"], true);
        assert_eq!(v["userID"], "u1");
        assert_eq!(v["oauthAccount"]["accountUuid"], "a1");
        assert!(v["oauthAccount"].get("emailAddress").is_none());
        assert_eq!(minimal_state(b"not json"), b"{}");
    }
}
