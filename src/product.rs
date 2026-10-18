//! Product identity and hosting constants.
//!
//! The GitHub owner/repo below is the single source of truth for every online
//! endpoint flockr talks to: the self-update manifests, release notes, product
//! announcements, remote installer downloads, agent-detection manifest fetches
//! and the docs links printed by the CLI. Change the owner or repo here (and in
//! `Cargo.toml` / `distribution/` metadata) to repoint a fork.
//!
//! The macros exist so the URLs can also be used inside `concat!` (for example
//! in static help text); the `pub(crate) const`s are the same values for
//! ordinary code.

macro_rules! github_owner {
    () => {
        "Rambarani"
    };
}
macro_rules! github_repo {
    () => {
        "flockr"
    };
}
/// `https://github.com/<owner>/<repo>`
macro_rules! repo_url {
    () => {
        concat!("https://github.com/", github_owner!(), "/", github_repo!())
    };
}
/// A file under `distribution/` on the default branch, served raw.
macro_rules! distribution_url {
    ($file:literal) => {
        concat!(
            "https://raw.githubusercontent.com/",
            github_owner!(),
            "/",
            github_repo!(),
            "/main/distribution/",
            $file
        )
    };
}
/// The docs source tree on the default branch.
macro_rules! docs_url {
    () => {
        concat!(
            "https://github.com/",
            github_owner!(),
            "/",
            github_repo!(),
            "/tree/main/docs/next/website/src/content/docs"
        )
    };
}

#[cfg(test)]
pub(crate) const GITHUB_OWNER: &str = github_owner!();
#[cfg(test)]
pub(crate) const GITHUB_REPO: &str = github_repo!();
pub(crate) const REPO_URL: &str = repo_url!();
/// Stable release channel manifest (`distribution/latest.json`).
pub(crate) const STABLE_UPDATE_MANIFEST_URL: &str = distribution_url!("latest.json");
/// Preview release channel manifest (`distribution/preview.json`).
pub(crate) const PREVIEW_UPDATE_MANIFEST_URL: &str = distribution_url!("preview.json");
/// Remotely published agent-detection manifest catalog.
pub(crate) const AGENT_DETECTION_CATALOG_URL: &str =
    distribution_url!("agent-detection/index.toml");
/// Homebrew formula metadata for Homebrew-managed installs. No flockr formula is
/// published yet, so the check fails soft until one exists.
pub(crate) const HOMEBREW_FORMULA_API_URL: &str = concat!(
    "https://formulae.brew.sh/api/formula/",
    github_repo!(),
    ".json"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_derived_from_owner_and_repo() {
        assert_eq!(
            REPO_URL,
            format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}")
        );
        for url in [
            STABLE_UPDATE_MANIFEST_URL,
            PREVIEW_UPDATE_MANIFEST_URL,
            AGENT_DETECTION_CATALOG_URL,
        ] {
            assert!(
                url.starts_with(&format!(
                    "https://raw.githubusercontent.com/{GITHUB_OWNER}/{GITHUB_REPO}/main/distribution/"
                )),
                "{url}"
            );
        }
        assert!(docs_url!().starts_with(REPO_URL));
    }
}
