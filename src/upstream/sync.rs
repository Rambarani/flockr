//! `flockr upstream sync <tag>`: prepare an isolated worktree for merging an
//! upstream herdr release and hand it to an agent.
//!
//! The sync never touches the main checkout's working tree or the `flockr`
//! branch. It only (a) adds a git remote named `upstream` when missing,
//! (b) fetches tags into the shared object store, and (c) creates a worktree
//! `<repo>/../flockr-sync-<tag>` on a new branch `upstream-sync/<tag>`.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::release::{bucket_release_body, parse_tag_version, UpstreamRelease};
use super::state::UpstreamState;
use super::sync_branch_name;

/// Branch that holds flockr's own work. Sync branches start from it.
pub(crate) const MAIN_BRANCH: &str = "flockr";
/// File inside a checkout that marks it as a flockr source tree.
pub(crate) const BASE_FILE: &str = ".flockr/upstream-base";

pub(crate) struct SyncOptions<'a> {
    pub repo: &'a Path,
    pub tag: &'a str,
    /// URL the `upstream` remote must point at (the herdr repository).
    pub remote_url: &'a str,
    pub agent_command: &'a str,
    /// Watcher state, used for the release notes included in the prompt.
    pub state: &'a UpstreamState,
}

/// Rejects tags that are not plain release versions. The tag ends up in branch
/// names, paths and shell-adjacent text, so only a conservative alphabet is
/// accepted.
pub(crate) fn validate_tag(tag: &str) -> Result<(), String> {
    let valid_chars = tag
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if tag.is_empty()
        || !valid_chars
        || tag.starts_with('-')
        || tag.starts_with('.')
        || parse_tag_version(tag).is_none()
    {
        return Err(format!(
            "invalid upstream tag {tag:?}: expected a release tag such as v0.9.7"
        ));
    }
    Ok(())
}

fn is_flockr_checkout(path: &Path) -> bool {
    path.join(".git").exists() && path.join(BASE_FILE).is_file()
}

/// Source checkout to sync in: `[upstream] repo_path`, else the checkout this
/// binary was built from when it still looks like a flockr checkout.
///
/// The compile-time default is only a convenience for binaries built from
/// source on the machine that uses them; a distributed binary has no such
/// directory and gets an explanation instead of a guess.
pub(crate) fn resolve_repo_path(configured: Option<&str>) -> Result<PathBuf, String> {
    resolve_repo_path_with(configured, Path::new(env!("CARGO_MANIFEST_DIR")))
}

fn resolve_repo_path_with(configured: Option<&str>, built_from: &Path) -> Result<PathBuf, String> {
    if let Some(path) = configured.map(str::trim).filter(|path| !path.is_empty()) {
        return Ok(crate::worktree::expand_tilde_absolute_path(path));
    }
    if is_flockr_checkout(built_from) {
        return Ok(built_from.to_path_buf());
    }
    Err(format!(
        "no flockr source checkout is configured. Set it in config.toml:\n\n    [upstream]\n    repo_path = \"/path/to/flockr\"\n\n(the checkout this binary was built from, {}, is not available)",
        built_from.display()
    ))
}

pub(crate) fn worktree_path(repo: &Path, tag: &str) -> PathBuf {
    repo.parent()
        .unwrap_or(repo)
        .join(format!("flockr-sync-{tag}"))
}

pub(crate) fn prompt_path(worktree: &Path, tag: &str) -> PathBuf {
    worktree.join(format!(".flockr/sync-{tag}.md"))
}

/// Marker the agent writes after the final commit once every check passed. It
/// holds the verified commit hash, so a later commit invalidates it.
pub(crate) fn marker_path(worktree: &Path, tag: &str) -> PathBuf {
    worktree.join(format!(".flockr/sync-{tag}.verified"))
}

pub(crate) fn run_git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git {} failed: {}", args.join(" "), stderr.trim()))
    }
}

pub(crate) fn git_succeeds(repo: &Path, args: &[&str]) -> bool {
    run_git(repo, args).is_ok()
}

pub(crate) fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Checks that `repo` is the top level of a git checkout that has the `flockr`
/// branch. Never modifies anything.
pub(crate) fn preflight_repo(repo: &Path) -> Result<(), String> {
    if !repo.exists() {
        return Err(format!(
            "upstream repo_path {} does not exist; set [upstream] repo_path to your flockr checkout",
            repo.display()
        ));
    }
    if !repo.is_dir() {
        return Err(format!(
            "upstream repo_path {} is not a directory",
            repo.display()
        ));
    }
    let toplevel = run_git(repo, &["rev-parse", "--show-toplevel"]).map_err(|_| {
        format!(
            "upstream repo_path {} is not a git repository",
            repo.display()
        )
    })?;
    if canonical(Path::new(&toplevel)) != canonical(repo) {
        return Err(format!(
            "upstream repo_path {} is inside a git repository but is not its top-level directory ({toplevel})",
            repo.display()
        ));
    }
    if !git_succeeds(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{MAIN_BRANCH}"),
        ],
    ) {
        return Err(format!(
            "git branch `{MAIN_BRANCH}` does not exist in {}; sync branches start from it",
            repo.display()
        ));
    }
    Ok(())
}

fn normalize_remote_url(url: &str) -> String {
    url.trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

/// Makes sure a remote named `upstream` points at the herdr repository. An
/// existing remote with another URL is never rewritten; the sync stops instead.
fn ensure_upstream_remote(repo: &Path, url: &str, out: &mut dyn Write) -> Result<(), String> {
    let name = crate::product::UPSTREAM_REMOTE_NAME;
    match run_git(repo, &["remote", "get-url", name]) {
        Ok(existing) => {
            if normalize_remote_url(&existing) != normalize_remote_url(url) {
                return Err(format!(
                    "git remote `{name}` already points at {existing}, not {url}; refusing to use or change it. Remove or rename that remote and run the sync again"
                ));
            }
            let _ = writeln!(out, "remote `{name}` -> {existing}");
        }
        Err(_) => {
            run_git(repo, &["remote", "add", name, url])?;
            let _ = writeln!(out, "added git remote `{name}` -> {url}");
        }
    }
    Ok(())
}

fn fetch_upstream(repo: &Path, out: &mut dyn Write) -> Result<(), String> {
    let name = crate::product::UPSTREAM_REMOTE_NAME;
    let _ = writeln!(out, "git fetch {name} --tags");
    run_git(repo, &["fetch", name, "--tags"]).map(|_| ())
}

pub(crate) fn tag_exists(repo: &Path, tag: &str) -> bool {
    git_succeeds(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{tag}^{{commit}}"),
        ],
    )
}

pub(crate) fn worktree_registered_on_branch(repo: &Path, path: &Path, branch: &str) -> bool {
    let Ok(list) = run_git(repo, &["worktree", "list", "--porcelain"]) else {
        return false;
    };
    let wanted_path = canonical(path);
    let wanted_branch = format!("branch refs/heads/{branch}");
    let mut current_path: Option<PathBuf> = None;
    for line in list.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(canonical(Path::new(path)));
        } else if line == wanted_branch && current_path.as_ref() == Some(&wanted_path) {
            return true;
        }
    }
    false
}

/// Creates (or resumes) the sync worktree. Returns whether it was newly created.
fn ensure_worktree(
    repo: &Path,
    path: &Path,
    branch: &str,
    out: &mut dyn Write,
) -> Result<bool, String> {
    let branch_exists = git_succeeds(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    );
    let path_str = path.to_string_lossy();
    if path.exists() {
        if worktree_registered_on_branch(repo, path, branch) {
            let _ = writeln!(out, "resuming existing worktree {}", path.display());
            return Ok(false);
        }
        return Err(format!(
            "{} already exists and is not a worktree of {} on branch {branch}; move it away and retry",
            path.display(),
            repo.display()
        ));
    }
    if branch_exists {
        run_git(repo, &["worktree", "add", &path_str, branch])?;
        let _ = writeln!(
            out,
            "created worktree {} on existing branch {branch}",
            path.display()
        );
    } else {
        run_git(
            repo,
            &["worktree", "add", "-b", branch, &path_str, MAIN_BRANCH],
        )?;
        let _ = writeln!(
            out,
            "created worktree {} on new branch {branch} (from {MAIN_BRANCH})",
            path.display()
        );
    }
    Ok(true)
}

/// Release notes for the tags newer than `base` up to and including `tag`,
/// taken from the watcher's cache (empty when the cache has none).
fn cached_notes(state: &UpstreamState, base_tag: &str, tag: &str) -> String {
    let (Some(base), Some(target)) = (parse_tag_version(base_tag), parse_tag_version(tag)) else {
        return String::new();
    };
    let mut releases: Vec<&UpstreamRelease> = state
        .pending
        .iter()
        .filter(|release| parse_tag_version(&release.tag).is_some_and(|v| v > base && v <= target))
        .collect();
    releases.sort_by_key(|release| parse_tag_version(&release.tag));
    let mut notes = String::new();
    for release in releases {
        notes.push_str(&format!("### {}\n\n", release.tag));
        let body = release.body.trim();
        if body.is_empty() || (bucket_release_body(body).is_empty() && body.len() > 4000) {
            notes.push_str("(see the release page)\n\n");
        } else {
            notes.push_str(body);
            notes.push_str("\n\n");
        }
    }
    notes
}

pub(crate) struct PromptInput<'a> {
    pub tag: &'a str,
    pub base_tag: &'a str,
    pub branch: &'a str,
    pub repo: &'a Path,
    pub worktree: &'a Path,
    pub release_notes: &'a str,
}

/// The instructions handed to the agent. Everything the agent must preserve
/// lives here so a sync does not depend on the agent knowing flockr's history.
pub(crate) fn build_prompt(input: &PromptInput<'_>) -> String {
    let PromptInput {
        tag,
        base_tag,
        branch,
        repo,
        worktree,
        release_notes,
    } = input;
    let repo = repo.display();
    let worktree = worktree.display();
    let notes = if release_notes.trim().is_empty() {
        format!("(no cached release notes; read `git log {base_tag}..{tag}` and the CHANGELOG)\n")
    } else {
        release_notes.trim().to_string() + "\n"
    };
    format!(
        r#"# Sync upstream herdr {tag} into flockr

You are working in a dedicated git worktree: `{worktree}`
Branch: `{branch}` (created from the `{MAIN_BRANCH}` branch).
The main checkout is `{repo}`. flockr is currently synced to upstream herdr `{base_tag}`; your job is to bring in herdr `{tag}`.
The upstream remote is `upstream` and the tag is already fetched.

## Hard rules

- Work only in this worktree, on branch `{branch}`.
- NEVER merge into, check out, reset, rebase or commit on the `{MAIN_BRANCH}` branch.
- NEVER push to any remote.
- NEVER touch the installed flockr binary. The `flockr` command on this machine is a symlink into the main checkout's `target/release`; build only in this worktree's own `target/` and do not copy anything over the installed binary.
- Do not modify files in the main checkout `{repo}`.
- Leave `.flockr/sync-{tag}.md` (this file) and `.flockr/sync-{tag}.verified` uncommitted.
- Do not run `flockr upstream apply`; applying the result is the user's decision.

## Steps

1. Merge the release: `git merge --no-ff --no-commit {tag}`. Review the incoming changes with `git log {base_tag}..{tag}` and the diff before resolving anything.
2. Resolve conflicts preserving flockr's changes. Always keep:
   - the rename: crate and binary name `flockr`, config/state/socket/log paths, the `FLOCKR_*` environment variables, `flockr:<id>` report sources and every other flockr name;
   - `src/legacy_herdr.rs` and its aliases (`HERDR_*` env twins, `herdr:<id>` report sources, deprecated `"herdr"` config values);
   - `src/product.rs` hosting URLs (flockr's own GitHub repository; do not reintroduce herdr.dev or herdrdev/herdr hosting endpoints other than the upstream watcher constants);
   - sidebar agents grouped under workspace headers;
   - configurable notification sounds and the tones settings tab;
   - the upstream watcher: `src/upstream/`, `src/cli/upstream.rs`, the `[upstream]` config section and `.flockr/`.
3. Apply the herdr -> flockr naming to everything newly added by upstream: new `HERDR_*` env vars become `FLOCKR_*` (read through `crate::legacy_herdr::var` so the `HERDR_*` spelling keeps working), and new ids, paths, strings, docs and tests that say herdr become flockr. Where third-party agents or scripts read a NEW `HERDR_*` name (pane runtime contract, hook scripts), add a legacy alias following the pattern in `src/legacy_herdr.rs`, with a test.
4. Update `.flockr/upstream-base` to contain exactly `{tag}` and a newline, and add a line to `NOTICE` recording the sync (tag, upstream commit hash, date).
5. Write `.flockr/parity-{tag}.md` (committed) listing every new upstream feature and fix you adopted: CLI commands and flags, config keys, environment variables, API methods and events, each with its source file. Use the format of `{repo}/docs/flockr/parity-checklist.md` as a reference. That checklist is gitignored; read it but do not edit it in the main checkout.
6. Verify inside this worktree. The vendored libghostty-vt build needs Zig 0.16.0 (set `ZIG` or put it on `PATH` if it is not already). All four must pass:
   - `cargo build --release`
   - `cargo nextest run --no-fail-fast` (use nextest; plain `cargo test` gives false environment failures)
   - `cargo clippy --all-targets --locked -- -D warnings`
   - `cargo fmt --check`
   Fix failures caused by the merge. If a failure also exists on the `{MAIN_BRANCH}` branch, record it as pre-existing instead of hiding it.
7. Commit everything except the two uncommitted files on `{branch}` with a lowercase conventional commit, for example `chore: sync upstream herdr {tag}`.
8. Only after the final commit and only if all four checks passed, record the verified commit: `git rev-parse HEAD > .flockr/sync-{tag}.verified`. If any check fails or any conflict is unresolved, do not write that file.
9. Finish with a summary: the new features and fixes adopted, conflicts and how you resolved them, anything you could not resolve, and the result of each check. Tell the user the next step is `flockr upstream apply {tag}`, to be run by them.

## Upstream release notes

{notes}"#,
    )
}

/// Shell command that starts the agent with the prompt. A `{prompt_file}`
/// placeholder expands to the quoted prompt path; otherwise the prompt text is
/// passed as the first argument, e.g. `claude "$(cat <file>)"`.
pub(crate) fn agent_shell_command(agent_command: &str, prompt: &Path) -> Result<String, String> {
    let agent_command = agent_command.trim();
    if agent_command.is_empty() {
        return Err("[upstream] agent_command is empty".into());
    }
    let quoted = shell_quote_path(prompt);
    if agent_command.contains("{prompt_file}") {
        return Ok(agent_command.replace("{prompt_file}", &quoted));
    }
    if cfg!(windows) {
        return Err(
            "on Windows [upstream] agent_command must contain a {prompt_file} placeholder".into(),
        );
    }
    Ok(format!("{agent_command} \"$(cat {quoted})\""))
}

pub(crate) fn shell_quote_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if cfg!(windows) {
        format!("\"{value}\"")
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Runs the sync. `launch` receives the worktree and the shell command for the
/// agent and returns its exit code (the real launcher replaces the process).
pub(crate) fn run_sync(
    options: &SyncOptions<'_>,
    launch: &mut dyn FnMut(&Path, &str) -> std::io::Result<i32>,
    out: &mut dyn Write,
) -> Result<i32, String> {
    validate_tag(options.tag)?;
    preflight_repo(options.repo)?;
    // Fail before any git mutation if the agent cannot be launched.
    let worktree = worktree_path(options.repo, options.tag);
    let prompt_file = prompt_path(&worktree, options.tag);
    let agent_cmd = agent_shell_command(options.agent_command, &prompt_file)?;

    ensure_upstream_remote(options.repo, options.remote_url, out)?;
    fetch_upstream(options.repo, out)?;
    if !tag_exists(options.repo, options.tag) {
        return Err(format!(
            "tag {} was not found on remote `{}` after fetching",
            options.tag,
            crate::product::UPSTREAM_REMOTE_NAME
        ));
    }

    let branch = sync_branch_name(options.tag);
    ensure_worktree(options.repo, &worktree, &branch, out)?;

    let base_tag = std::fs::read_to_string(worktree.join(BASE_FILE))
        .map(|content| content.trim().to_string())
        .unwrap_or_else(|_| super::compiled_base_tag().to_string());
    let notes = cached_notes(options.state, &base_tag, options.tag);
    let prompt = build_prompt(&PromptInput {
        tag: options.tag,
        base_tag: &base_tag,
        branch: &branch,
        repo: options.repo,
        worktree: &worktree,
        release_notes: &notes,
    });
    if let Some(parent) = prompt_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&prompt_file, prompt)
        .map_err(|e| format!("failed to write {}: {e}", prompt_file.display()))?;
    let _ = writeln!(out, "wrote prompt {}", prompt_file.display());
    let _ = writeln!(
        out,
        "launching agent in {}: {agent_cmd}",
        worktree.display()
    );
    let _ = out.flush();

    launch(&worktree, &agent_cmd).map_err(|e| format!("failed to launch agent: {e}"))
}

/// Replaces this process with the agent shell command (Unix) so the agent owns
/// the terminal pane, or runs it and waits (elsewhere).
#[cfg(unix)]
pub(crate) fn exec_agent(cwd: &Path, command: &str) -> std::io::Result<i32> {
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .exec();
    Err(error)
}

#[cfg(not(unix))]
pub(crate) fn exec_agent(cwd: &Path, command: &str) -> std::io::Result<i32> {
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg(command)
        .current_dir(cwd)
        .status()?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub(crate) fn unique_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flockr-upstream-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    /// Runs git with a fixed identity and without reading global config.
    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=flockr-test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A fake "herdr" upstream repo (tags v9.9.8 and v9.9.9) and a flockr
    /// checkout on branch `flockr` containing `.flockr/upstream-base`.
    pub(crate) struct Fixture {
        pub root: PathBuf,
        pub upstream: PathBuf,
        pub repo: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new(name: &str) -> Self {
            let root = unique_dir(name);
            let upstream = root.join("herdr-upstream");
            std::fs::create_dir_all(&upstream).unwrap();
            git(&upstream, &["init", "-q", "-b", "main"]);
            std::fs::write(upstream.join("herdr.txt"), "herdr\n").unwrap();
            git(&upstream, &["add", "."]);
            git(&upstream, &["commit", "-q", "-m", "init"]);
            git(&upstream, &["tag", "v9.9.8"]);
            std::fs::write(upstream.join("feature.txt"), "new\n").unwrap();
            git(&upstream, &["add", "."]);
            git(&upstream, &["commit", "-q", "-m", "feat: new"]);
            git(&upstream, &["tag", "v9.9.9"]);

            let repo = root.join("flockr");
            std::fs::create_dir_all(repo.join(".flockr")).unwrap();
            git(&repo, &["init", "-q", "-b", "flockr"]);
            std::fs::write(repo.join(super::BASE_FILE), "v9.9.8\n").unwrap();
            std::fs::write(repo.join("README.md"), "flockr\n").unwrap();
            git(&repo, &["add", "."]);
            git(&repo, &["commit", "-q", "-m", "init flockr"]);
            Self {
                root,
                upstream,
                repo,
            }
        }

        pub(crate) fn upstream_url(&self) -> String {
            self.upstream.to_string_lossy().into_owned()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{git, unique_dir, Fixture};
    use super::*;

    type Launches = Vec<(PathBuf, String)>;

    fn sync(
        fixture: &Fixture,
        tag: &str,
        agent_command: &str,
    ) -> (Result<i32, String>, Launches, String) {
        let url = fixture.upstream_url();
        let state = UpstreamState::default();
        let options = SyncOptions {
            repo: &fixture.repo,
            tag,
            remote_url: &url,
            agent_command,
            state: &state,
        };
        let mut launches = Vec::new();
        let mut out = Vec::new();
        let result = run_sync(
            &options,
            &mut |cwd, cmd| {
                launches.push((cwd.to_path_buf(), cmd.to_string()));
                Ok(0)
            },
            &mut out,
        );
        (result, launches, String::from_utf8(out).unwrap())
    }

    #[test]
    fn tag_validation_accepts_releases_and_rejects_injection() {
        assert!(validate_tag("v0.9.7").is_ok());
        assert!(validate_tag("0.9.7").is_ok());
        assert!(validate_tag("v0.9.7-rc.1").is_ok());
        for bad in [
            "",
            "-v0.9.7",
            ".hidden",
            "v0.9.7;rm",
            "v0.9.7 x",
            "../v0.9.7",
            "v0.9.7/..",
            "preview-2026-10-14-abc",
            "main",
            "$(id)",
        ] {
            assert!(validate_tag(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn repo_path_prefers_config_then_build_checkout_then_explains() {
        let root = unique_dir("resolve");
        let checkout = root.join("src-checkout");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        std::fs::create_dir_all(checkout.join(".flockr")).unwrap();
        std::fs::write(checkout.join(BASE_FILE), "v0.9.5\n").unwrap();

        assert_eq!(
            resolve_repo_path_with(Some("/somewhere/else"), &checkout).unwrap(),
            PathBuf::from("/somewhere/else")
        );
        assert_eq!(
            resolve_repo_path_with(Some("  "), &checkout).unwrap(),
            checkout
        );
        assert_eq!(resolve_repo_path_with(None, &checkout).unwrap(), checkout);

        let missing = root.join("gone");
        let err = resolve_repo_path_with(None, &missing).unwrap_err();
        assert!(err.contains("repo_path"), "{err}");
        assert!(err.contains("[upstream]"), "{err}");

        // A directory without the flockr marker is not trusted as a default.
        let plain = root.join("plain");
        std::fs::create_dir_all(plain.join(".git")).unwrap();
        assert!(resolve_repo_path_with(None, &plain).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preflight_refuses_missing_path_non_git_dir_and_missing_branch() {
        let root = unique_dir("preflight");

        let missing = root.join("missing");
        let err = preflight_repo(&missing).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");

        let file = root.join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(preflight_repo(&file)
            .unwrap_err()
            .contains("not a directory"));

        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let err = preflight_repo(&plain).unwrap_err();
        assert!(err.contains("not a git repository"), "{err}");

        let other_branch = root.join("other");
        std::fs::create_dir_all(&other_branch).unwrap();
        git(&other_branch, &["init", "-q", "-b", "main"]);
        std::fs::write(other_branch.join("a"), "a").unwrap();
        git(&other_branch, &["add", "."]);
        git(&other_branch, &["commit", "-q", "-m", "a"]);
        let err = preflight_repo(&other_branch).unwrap_err();
        assert!(err.contains("`flockr` does not exist"), "{err}");

        let subdir = other_branch.join("sub");
        std::fs::create_dir_all(&subdir).unwrap();
        let err = preflight_repo(&subdir).unwrap_err();
        assert!(err.contains("not its top-level"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_refuses_before_touching_git_for_bad_inputs() {
        let fixture = Fixture::new("sync-refuse");
        let (result, launches, _) = sync(&fixture, "main", "cat {prompt_file}");
        assert!(result.unwrap_err().contains("invalid upstream tag"));
        assert!(launches.is_empty());

        let (result, _, _) = sync(&fixture, "v9.9.9", "  ");
        assert!(result.unwrap_err().contains("agent_command is empty"));
        // No remote, no worktree, nothing changed.
        assert!(git(&fixture.repo, &["remote"]).is_empty());
        assert!(!worktree_path(&fixture.repo, "v9.9.9").exists());
    }

    #[test]
    fn sync_creates_isolated_worktree_and_leaves_main_checkout_untouched() {
        let fixture = Fixture::new("sync-e2e");
        let head_before = git(&fixture.repo, &["rev-parse", "HEAD"]);
        let flockr_before = git(&fixture.repo, &["rev-parse", "refs/heads/flockr"]);

        let (result, launches, out) = sync(&fixture, "v9.9.9", "cat {prompt_file}");
        assert_eq!(result.unwrap(), 0, "{out}");

        // Main checkout: same branch, same commit, clean tree, no extra files.
        assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            git(&fixture.repo, &["rev-parse", "refs/heads/flockr"]),
            flockr_before
        );
        assert_eq!(git(&fixture.repo, &["branch", "--show-current"]), "flockr");
        assert!(git(&fixture.repo, &["status", "--porcelain"]).is_empty());
        assert_eq!(
            git(&fixture.repo, &["remote", "get-url", "upstream"]),
            fixture.upstream_url()
        );
        assert!(git(&fixture.repo, &["remote"])
            .lines()
            .all(|remote| remote == "upstream"));

        // Worktree on the sync branch, starting at flockr.
        let worktree = worktree_path(&fixture.repo, "v9.9.9");
        assert_eq!(worktree, fixture.root.join("flockr-sync-v9.9.9"));
        assert!(worktree.join("README.md").is_file());
        assert_eq!(
            git(&worktree, &["branch", "--show-current"]),
            "upstream-sync/v9.9.9"
        );
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), flockr_before);
        assert!(
            !worktree.join("feature.txt").exists(),
            "the tag is merged by the agent, not by sync"
        );

        // Prompt written into the worktree and handed to the agent.
        let prompt = std::fs::read_to_string(prompt_path(&worktree, "v9.9.9")).unwrap();
        assert!(prompt.contains("git merge --no-ff --no-commit v9.9.9"));
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].0, worktree);
        assert_eq!(
            launches[0].1,
            format!("cat '{}'", prompt_path(&worktree, "v9.9.9").display())
        );
    }

    #[test]
    fn sync_resumes_existing_worktree_and_rewrites_prompt() {
        let fixture = Fixture::new("sync-resume");
        let (first, _, _) = sync(&fixture, "v9.9.9", "cat {prompt_file}");
        first.unwrap();
        let worktree = worktree_path(&fixture.repo, "v9.9.9");
        std::fs::write(prompt_path(&worktree, "v9.9.9"), "stale").unwrap();

        let (second, launches, out) = sync(&fixture, "v9.9.9", "cat {prompt_file}");
        second.unwrap();
        assert!(out.contains("resuming existing worktree"), "{out}");
        assert_eq!(launches.len(), 1);
        let prompt = std::fs::read_to_string(prompt_path(&worktree, "v9.9.9")).unwrap();
        assert!(prompt.starts_with("# Sync upstream herdr v9.9.9"));
    }

    #[test]
    fn sync_refuses_foreign_directory_at_worktree_path() {
        let fixture = Fixture::new("sync-foreign");
        let worktree = worktree_path(&fixture.repo, "v9.9.9");
        std::fs::create_dir_all(&worktree).unwrap();
        let (result, launches, _) = sync(&fixture, "v9.9.9", "cat {prompt_file}");
        assert!(result.unwrap_err().contains("not a worktree"));
        assert!(launches.is_empty());
        assert!(git(&fixture.repo, &["branch", "--list", "upstream-sync/*"]).is_empty());
    }

    #[test]
    fn sync_refuses_remote_with_other_url_and_never_rewrites_it() {
        let fixture = Fixture::new("sync-remote");
        git(
            &fixture.repo,
            &[
                "remote",
                "add",
                "upstream",
                "https://example.invalid/fork.git",
            ],
        );
        let (result, launches, _) = sync(&fixture, "v9.9.9", "cat {prompt_file}");
        let err = result.unwrap_err();
        assert!(err.contains("already points at"), "{err}");
        assert!(launches.is_empty());
        assert_eq!(
            git(&fixture.repo, &["remote", "get-url", "upstream"]),
            "https://example.invalid/fork.git"
        );
        assert!(!worktree_path(&fixture.repo, "v9.9.9").exists());
    }

    #[test]
    fn sync_reports_tags_missing_upstream() {
        let fixture = Fixture::new("sync-notag");
        let (result, launches, _) = sync(&fixture, "v9.9.10", "cat {prompt_file}");
        let err = result.unwrap_err();
        assert!(err.contains("was not found on remote"), "{err}");
        assert!(launches.is_empty());
        assert!(!worktree_path(&fixture.repo, "v9.9.10").exists());
    }

    #[test]
    fn remote_url_comparison_ignores_suffix_case_and_slash() {
        assert_eq!(
            normalize_remote_url("https://github.com/herdrdev/herdr.git/"),
            normalize_remote_url("https://github.com/HerdrDev/herdr")
        );
        assert_ne!(
            normalize_remote_url("https://github.com/herdrdev/herdr"),
            normalize_remote_url("https://github.com/Rambarani/flockr")
        );
    }

    #[test]
    fn agent_command_supports_placeholder_and_default_argument_form() {
        let prompt = Path::new("/w/.flockr/sync-v1.0.0.md");
        assert_eq!(
            agent_shell_command("cat {prompt_file}", prompt).unwrap(),
            "cat '/w/.flockr/sync-v1.0.0.md'"
        );
        assert_eq!(
            agent_shell_command("my-agent --prompt-file {prompt_file} --yes", prompt).unwrap(),
            "my-agent --prompt-file '/w/.flockr/sync-v1.0.0.md' --yes"
        );
        if !cfg!(windows) {
            assert_eq!(
                agent_shell_command("claude", prompt).unwrap(),
                "claude \"$(cat '/w/.flockr/sync-v1.0.0.md')\""
            );
            assert_eq!(
                agent_shell_command("claude", Path::new("/it's here/p.md")).unwrap(),
                "claude \"$(cat '/it'\\''s here/p.md')\""
            );
        }
        assert!(agent_shell_command("", prompt).is_err());
    }

    #[test]
    fn prompt_carries_the_preservation_rules_and_never_merge_constraints() {
        let notes = "### v0.9.7\n\n### Added\n- thing\n\n";
        let prompt = build_prompt(&PromptInput {
            tag: "v0.9.7",
            base_tag: "v0.9.5",
            branch: "upstream-sync/v0.9.7",
            repo: Path::new("/src/flockr"),
            worktree: Path::new("/src/flockr-sync-v0.9.7"),
            release_notes: notes,
        });
        for needle in [
            "# Sync upstream herdr v0.9.7 into flockr",
            "`upstream-sync/v0.9.7`",
            "git merge --no-ff --no-commit v0.9.7",
            "git log v0.9.5..v0.9.7",
            "FLOCKR_*",
            "src/legacy_herdr.rs",
            "src/product.rs",
            "sidebar agents grouped under workspace headers",
            "configurable notification sounds",
            "src/upstream/",
            "legacy alias",
            ".flockr/upstream-base",
            "NOTICE",
            ".flockr/parity-v0.9.7.md",
            "cargo build --release",
            "cargo nextest run",
            "cargo clippy --all-targets --locked -- -D warnings",
            "cargo fmt --check",
            "NEVER merge into, check out, reset, rebase or commit on the `flockr` branch",
            "NEVER push to any remote",
            "NEVER touch the installed flockr binary",
            "git rev-parse HEAD > .flockr/sync-v0.9.7.verified",
            "flockr upstream apply v0.9.7",
            "- thing",
        ] {
            assert!(prompt.contains(needle), "prompt is missing {needle:?}");
        }
    }

    #[test]
    fn cached_notes_cover_releases_between_base_and_target() {
        let release = |tag: &str, body: &str| UpstreamRelease {
            tag: tag.to_string(),
            name: String::new(),
            body: body.to_string(),
            prerelease: false,
            published_at: None,
            url: String::new(),
        };
        let state = UpstreamState {
            pending: vec![
                release("v0.9.8", "### Added\n- later"),
                release("v0.9.6", "### Fixed\n- a"),
                release("v0.9.7", "### Added\n- b"),
            ],
            ..Default::default()
        };
        let notes = cached_notes(&state, "v0.9.5", "v0.9.7");
        assert!(notes.find("v0.9.6").unwrap() < notes.find("v0.9.7").unwrap());
        assert!(notes.contains("- a") && notes.contains("- b"));
        assert!(!notes.contains("later"));
        assert!(cached_notes(&state, "garbage", "v0.9.7").is_empty());
    }
}
