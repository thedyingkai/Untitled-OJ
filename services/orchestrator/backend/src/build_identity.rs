use anyhow::{Result, anyhow};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RuntimeProfile {
    Production,
    Desktop,
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BuildIdentity {
    pub(crate) version: &'static str,
    pub(crate) commit_sha: &'static str,
    pub(crate) profile: RuntimeProfile,
    pub(crate) target: &'static str,
}

impl BuildIdentity {
    pub(crate) fn compiled(profile: RuntimeProfile) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            commit_sha: env!("OJOS_BUILD_COMMIT"),
            profile,
            target: env!("OJOS_BUILD_TARGET"),
        }
    }

    pub(crate) fn require_production_commit(&self) -> Result<()> {
        if self.profile != RuntimeProfile::Production {
            return Ok(());
        }
        if is_canonical_commit(self.commit_sha) {
            return Ok(());
        }
        Err(anyhow!(
            "production PostgreSQL mode requires a build injected with a 40-character Git commit through OJOS_BUILD_COMMIT or GITHUB_SHA"
        ))
    }
}

fn is_canonical_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
