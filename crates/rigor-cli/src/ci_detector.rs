//! Runtime CI-environment detection (reference ADR-51 WD7), ported from
//! `lib/rigor/cli/ci_detector.rb`. Reads the well-known environment variables a
//! CI provider sets and returns the matching `Platform`, classified into a
//! **tier** that decides how `rigor check` surfaces diagnostics there:
//!
//!   `NativeStdout`   — rigor has a native format that renders purely from
//!                      stdout, auto-emitted on top of the human output
//!                      (GitHub Actions → `github`, TeamCity → `teamcity`).
//!   `NativeArtifact` — native format, but it needs a CI-wired report artifact,
//!                      not stdout (GitLab CI → `gitlab`); rigor only *hints*.
//!   `Reviewdog`      — no native rigor format; routed through reviewdog
//!                      (`checkstyle`/`sarif`) or `junit`. Hint only.
//!
//! Detection is a pure function of the environment, so it is fully testable; the
//! CLI passes the process env. `RIGOR_CI_DETECT=0` (or `false`/`no`/`off`)
//! disables it globally — the seam used for determinism (and so the differential
//! harness is never auto-augmented).

/// How a detected CI platform wants diagnostics surfaced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    NativeStdout,
    NativeArtifact,
    Reviewdog,
}

/// A detected CI platform: its id/name, the rigor format that fits it (None for
/// reviewdog-routed providers with no native format), and its tier.
#[derive(Clone, Debug)]
pub struct Platform {
    /// The provider id (e.g. `github-actions`). Carried for parity with the
    /// reference's `Platform`; asserted in tests, not read on the hot path.
    #[allow(dead_code)]
    pub id: &'static str,
    pub name: &'static str,
    pub format: Option<&'static str>,
    pub tier: Tier,
}

/// How a provider's env var is matched.
#[derive(Clone, Copy)]
enum Match {
    /// Value in {1, true, yes, on}.
    Truthy,
    /// Variable set non-empty.
    Present,
    /// Value equals a fixed string (case-insensitive).
    Equals(&'static str),
}

struct Provider {
    id: &'static str,
    name: &'static str,
    format: Option<&'static str>,
    tier: Tier,
    var: &'static str,
    matcher: Match,
}

/// The detection table, ordered most-specific first so the generic `CI=true`
/// catch-all is last (a provider that also sets `CI` is still recognised by its
/// own variable). Mirrors the reference's `PROVIDERS` row-for-row.
const PROVIDERS: &[Provider] = &[
    Provider { id: "github-actions", name: "GitHub Actions", format: Some("github"), tier: Tier::NativeStdout, var: "GITHUB_ACTIONS", matcher: Match::Truthy },
    Provider { id: "gitlab", name: "GitLab CI", format: Some("gitlab"), tier: Tier::NativeArtifact, var: "GITLAB_CI", matcher: Match::Truthy },
    Provider { id: "teamcity", name: "TeamCity", format: Some("teamcity"), tier: Tier::NativeStdout, var: "TEAMCITY_VERSION", matcher: Match::Present },
    Provider { id: "circleci", name: "CircleCI", format: None, tier: Tier::Reviewdog, var: "CIRCLECI", matcher: Match::Truthy },
    Provider { id: "jenkins", name: "Jenkins", format: None, tier: Tier::Reviewdog, var: "JENKINS_URL", matcher: Match::Present },
    Provider { id: "travis", name: "Travis CI", format: None, tier: Tier::Reviewdog, var: "TRAVIS", matcher: Match::Truthy },
    Provider { id: "appveyor", name: "AppVeyor", format: None, tier: Tier::Reviewdog, var: "APPVEYOR", matcher: Match::Truthy },
    Provider { id: "azure-pipelines", name: "Azure Pipelines", format: None, tier: Tier::Reviewdog, var: "TF_BUILD", matcher: Match::Present },
    Provider { id: "bitbucket", name: "Bitbucket Pipelines", format: None, tier: Tier::Reviewdog, var: "BITBUCKET_BUILD_NUMBER", matcher: Match::Present },
    Provider { id: "buildkite", name: "Buildkite", format: None, tier: Tier::Reviewdog, var: "BUILDKITE", matcher: Match::Truthy },
    Provider { id: "drone", name: "Drone CI", format: None, tier: Tier::Reviewdog, var: "DRONE", matcher: Match::Truthy },
    Provider { id: "semaphore", name: "Semaphore", format: None, tier: Tier::Reviewdog, var: "SEMAPHORE", matcher: Match::Truthy },
    Provider { id: "codeship", name: "Codeship", format: None, tier: Tier::Reviewdog, var: "CI_NAME", matcher: Match::Equals("codeship") },
    Provider { id: "ci", name: "CI", format: None, tier: Tier::Reviewdog, var: "CI", matcher: Match::Truthy },
];

/// Look up an env var via a caller-supplied getter (so detection stays a pure,
/// testable function of the environment).
type EnvFn<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Returns the detected `Platform`, or None when no CI is recognised or
/// detection is disabled via `RIGOR_CI_DETECT`. Uses the process environment.
pub fn detect() -> Option<Platform> {
    detect_with(&|key| std::env::var(key).ok())
}

/// Pure variant: detection over an arbitrary env getter (the spec seam).
pub fn detect_with(env: &EnvFn) -> Option<Platform> {
    if disabled(env) {
        return None;
    }
    let provider = PROVIDERS.iter().find(|p| matches(env, p))?;
    Some(Platform {
        id: provider.id,
        name: provider.name,
        format: provider.format,
        tier: provider.tier,
    })
}

fn disabled(env: &EnvFn) -> bool {
    let value = env("RIGOR_CI_DETECT")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(value.as_str(), "0" | "false" | "no" | "off")
}

fn matches(env: &EnvFn, provider: &Provider) -> bool {
    let value = env(provider.var).unwrap_or_default();
    let value = value.trim();
    match provider.matcher {
        Match::Truthy => {
            matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        }
        Match::Present => !value.is_empty(),
        Match::Equals(expected) => value.to_ascii_lowercase() == expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an env getter from a static (key, value) table.
    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn github_actions_is_native_stdout_github() {
        let p = detect_with(&env_of(&[("GITHUB_ACTIONS", "true")])).unwrap();
        assert_eq!(p.id, "github-actions");
        assert_eq!(p.format, Some("github"));
        assert_eq!(p.tier, Tier::NativeStdout);
    }

    #[test]
    fn gitlab_is_native_artifact_gitlab() {
        let p = detect_with(&env_of(&[("GITLAB_CI", "true")])).unwrap();
        assert_eq!(p.id, "gitlab");
        assert_eq!(p.format, Some("gitlab"));
        assert_eq!(p.tier, Tier::NativeArtifact);
    }

    #[test]
    fn teamcity_present_match() {
        // :present — any non-empty value triggers, even a version string.
        let p = detect_with(&env_of(&[("TEAMCITY_VERSION", "2024.03")])).unwrap();
        assert_eq!(p.id, "teamcity");
        assert_eq!(p.format, Some("teamcity"));
        assert_eq!(p.tier, Tier::NativeStdout);
    }

    #[test]
    fn reviewdog_provider_has_no_format() {
        let p = detect_with(&env_of(&[("JENKINS_URL", "http://ci")])).unwrap();
        assert_eq!(p.id, "jenkins");
        assert_eq!(p.format, None);
        assert_eq!(p.tier, Tier::Reviewdog);
    }

    #[test]
    fn codeship_equals_match() {
        assert!(detect_with(&env_of(&[("CI_NAME", "codeship")])).is_some());
        assert!(detect_with(&env_of(&[("CI_NAME", "other")])).is_none());
    }

    #[test]
    fn most_specific_wins_over_generic_ci() {
        // GitHub Actions also sets CI=true; the specific row must win.
        let p = detect_with(&env_of(&[("CI", "true"), ("GITHUB_ACTIONS", "true")])).unwrap();
        assert_eq!(p.id, "github-actions");
    }

    #[test]
    fn generic_ci_is_last_resort() {
        let p = detect_with(&env_of(&[("CI", "true")])).unwrap();
        assert_eq!(p.id, "ci");
        assert_eq!(p.format, None);
    }

    #[test]
    fn no_ci_no_platform() {
        assert!(detect_with(&env_of(&[])).is_none());
    }

    #[test]
    fn truthy_only_for_truthy_values() {
        assert!(detect_with(&env_of(&[("GITHUB_ACTIONS", "false")])).is_none());
        assert!(detect_with(&env_of(&[("GITHUB_ACTIONS", "")])).is_none());
        assert!(detect_with(&env_of(&[("GITHUB_ACTIONS", "1")])).is_some());
    }

    #[test]
    fn rigor_ci_detect_disables() {
        let env = env_of(&[("GITHUB_ACTIONS", "true"), ("RIGOR_CI_DETECT", "0")]);
        assert!(detect_with(&env).is_none());
        let env = env_of(&[("GITHUB_ACTIONS", "true"), ("RIGOR_CI_DETECT", "false")]);
        assert!(detect_with(&env).is_none());
        let env = env_of(&[("GITHUB_ACTIONS", "true"), ("RIGOR_CI_DETECT", "no")]);
        assert!(detect_with(&env).is_none());
    }
}
