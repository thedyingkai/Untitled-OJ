use anyhow::{Result, anyhow};
use serde::Serialize;

#[cfg(test)]
const DEVELOPMENT_COMMIT: &str = "development";

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

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(profile: RuntimeProfile, commit_sha: &'static str) -> BuildIdentity {
        BuildIdentity {
            version: "1.0.0",
            commit_sha,
            profile,
            target: "x86_64-unknown-linux-gnu",
        }
    }

    #[test]
    fn production_requires_a_canonical_full_commit() {
        assert!(
            identity(
                RuntimeProfile::Production,
                "0123456789abcdef0123456789abcdef01234567"
            )
            .require_production_commit()
            .is_ok()
        );
        for invalid in [
            DEVELOPMENT_COMMIT,
            "0123456789abcdef0123456789abcdef0123456",
            "0123456789abcdef0123456789abcdef0123456g",
            "0123456789ABCDEF0123456789ABCDEF01234567",
        ] {
            assert!(
                identity(RuntimeProfile::Production, invalid)
                    .require_production_commit()
                    .is_err(),
                "production unexpectedly accepted {invalid}"
            );
        }
    }

    #[test]
    fn development_identity_is_allowed_only_outside_production() {
        for profile in [RuntimeProfile::Desktop, RuntimeProfile::Ephemeral] {
            assert!(
                identity(profile, DEVELOPMENT_COMMIT)
                    .require_production_commit()
                    .is_ok()
            );
        }
    }
}
