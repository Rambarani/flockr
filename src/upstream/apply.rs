//! `flockr upstream apply <tag>`: merge a finished, verified sync branch into
//! `flockr`. Only ever runs on explicit user request.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::state::UpstreamState;
use super::sync::{
    canonical, git_succeeds, marker_path, preflight_repo, run_git, validate_tag, worktree_path,
    worktree_registered_on_branch, BASE_FILE, MAIN_BRANCH,
};
use super::sync_branch_name;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyPlan {
    pub repo: PathBuf,
    pub tag: String,
    pub branch: String,
    pub worktree: PathBuf,
    /// Commit of `flockr` before the apply (for the rollback hint).
    pub flockr_head: String,
    /// Commit of the sync branch that will be merged.
    pub branch_head: String,
    /// `flockr` is an ancestor of the sync branch, so no merge commit is needed.
    pub fast_forward: bool,
}

/// Paths in the sync worktree that are expected to stay uncommitted.
fn is_sync_scratch_file(status_line: &str, tag: &str) -> bool {
    let path = status_line.get(3..).unwrap_or("").trim();
    path == format!(".flockr/sync-{tag}.md") || path == format!(".flockr/sync-{tag}.verified")
}

/// Verifies every precondition for applying `tag` and returns what would be
/// done. Never modifies anything.
pub(crate) fn plan_apply(repo: &Path, tag: &str) -> Result<ApplyPlan, String> {
    validate_tag(tag)?;
    preflight_repo(repo)?;
    let branch = sync_branch_name(tag);

    if !git_succeeds(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    ) {
        return Err(format!(
            "sync branch {branch} does not exist; run `flockr upstream sync {tag}` first"
        ));
    }
    let branch_head = run_git(repo, &["rev-parse", &format!("refs/heads/{branch}")])?;

    let worktree = worktree_path(repo, tag);
    if !worktree_registered_on_branch(repo, &worktree, &branch) {
        return Err(format!(
            "sync worktree {} is missing or not on {branch}; the verification marker lives there",
            worktree.display()
        ));
    }

    let marker = marker_path(&worktree, tag);
    let recorded = std::fs::read_to_string(&marker).map_err(|_| {
        format!(
            "no recorded passing tests: {} does not exist. The sync agent writes it only after build, tests, clippy and fmt pass; finish the sync first",
            marker.display()
        )
    })?;
    if recorded.trim() != branch_head {
        return Err(format!(
            "the recorded passing tests are for commit {}, but {branch} is at {branch_head}; re-run the checks on the current commit and refresh {}",
            recorded.trim(),
            marker.display()
        ));
    }

    let current_branch = run_git(repo, &["branch", "--show-current"])?;
    if current_branch != MAIN_BRANCH {
        return Err(format!(
            "the main checkout is on `{current_branch}`, not `{MAIN_BRANCH}`; check out `{MAIN_BRANCH}` first"
        ));
    }
    let main_status = run_git(repo, &["status", "--porcelain"])?;
    if !main_status.is_empty() {
        return Err(format!(
            "the main checkout has uncommitted changes; commit or stash them first:\n{main_status}"
        ));
    }

    let worktree_status = run_git(&worktree, &["status", "--porcelain"])?;
    let unexpected: Vec<&str> = worktree_status
        .lines()
        .filter(|line| !is_sync_scratch_file(line, tag))
        .collect();
    if !unexpected.is_empty() {
        return Err(format!(
            "the sync worktree has uncommitted changes; commit them on {branch} (and re-verify) first:\n{}",
            unexpected.join("\n")
        ));
    }

    let base_in_branch = run_git(repo, &["show", &format!("{branch}:{BASE_FILE}")])
        .map_err(|_| format!("{BASE_FILE} is missing on {branch}"))?;
    if base_in_branch.trim() != tag {
        return Err(format!(
            "{BASE_FILE} on {branch} says {:?}, expected {tag:?}; the sync must record the new upstream base",
            base_in_branch.trim()
        ));
    }

    if !git_succeeds(
        repo,
        &[
            "merge-base",
            "--is-ancestor",
            &format!("refs/tags/{tag}"),
            &format!("refs/heads/{branch}"),
        ],
    ) {
        return Err(format!(
            "upstream tag {tag} is not merged into {branch}; the sync is incomplete"
        ));
    }

    let flockr_head = run_git(repo, &["rev-parse", &format!("refs/heads/{MAIN_BRANCH}")])?;
    let fast_forward = git_succeeds(
        repo,
        &[
            "merge-base",
            "--is-ancestor",
            &format!("refs/heads/{MAIN_BRANCH}"),
            &format!("refs/heads/{branch}"),
        ],
    );

    Ok(ApplyPlan {
        repo: repo.to_path_buf(),
        tag: tag.to_string(),
        branch,
        worktree,
        flockr_head,
        branch_head,
        fast_forward,
    })
}

/// Human-readable description of exactly what `execute_apply` will do.
pub(crate) fn describe_plan(plan: &ApplyPlan) -> Vec<String> {
    let merge_step = if plan.fast_forward {
        format!(
            "fast-forward `{MAIN_BRANCH}` to {} ({})",
            plan.branch,
            short(&plan.branch_head)
        )
    } else {
        format!(
            "merge {} ({}) into `{MAIN_BRANCH}` with a merge commit (`{MAIN_BRANCH}` has moved since the sync started)",
            plan.branch,
            short(&plan.branch_head)
        )
    };
    vec![
        format!("flockr upstream apply {} will:", plan.tag),
        format!("  1. {merge_step} in {}", plan.repo.display()),
        "  2. run `cargo build --release` there (this rebuilds target/release/flockr in that checkout)"
            .to_string(),
        format!(
            "  3. remove the sync worktree {} and delete branch {}",
            plan.worktree.display(),
            plan.branch
        ),
        format!("  4. record {} as the new upstream base", plan.tag),
        format!(
            "Roll back the merge with: git -C {} reset --hard {}",
            plan.repo.display(),
            plan.flockr_head
        ),
    ]
}

fn short(hash: &str) -> &str {
    hash.get(..9).unwrap_or(hash)
}

/// Merges the sync branch, builds, cleans up and records the new base.
///
/// `build` runs the release build in the repo and returns whether it passed. A
/// failed build keeps the merge and the worktree and reports the rollback
/// command; nothing is recorded.
pub(crate) fn execute_apply(
    plan: &ApplyPlan,
    state_path: &Path,
    build: &mut dyn FnMut(&Path) -> std::io::Result<bool>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let repo = plan.repo.as_path();
    if plan.fast_forward {
        run_git(repo, &["merge", "--ff-only", &plan.branch])?;
        let _ = writeln!(out, "fast-forwarded {MAIN_BRANCH} to {}", plan.branch);
    } else {
        let message = format!(
            "chore: merge upstream herdr {} into {MAIN_BRANCH}",
            plan.tag
        );
        if let Err(err) = run_git(
            repo,
            &[
                "merge",
                "--no-ff",
                "--no-verify",
                "-m",
                &message,
                &plan.branch,
            ],
        ) {
            let _ = run_git(repo, &["merge", "--abort"]);
            return Err(format!(
                "{err}\nthe merge was aborted and `{MAIN_BRANCH}` is unchanged"
            ));
        }
        let _ = writeln!(out, "merged {} into {MAIN_BRANCH}", plan.branch);
    }

    let _ = writeln!(out, "cargo build --release (in {})", repo.display());
    let _ = out.flush();
    match build(repo) {
        Ok(true) => {}
        Ok(false) | Err(_) => {
            return Err(format!(
                "cargo build --release failed after the merge. The merge is kept and the sync worktree {} is left in place. To undo the merge: git -C {} reset --hard {}",
                plan.worktree.display(),
                repo.display(),
                plan.flockr_head
            ));
        }
    }

    let worktree = plan.worktree.to_string_lossy();
    match run_git(repo, &["worktree", "remove", "--force", &worktree]) {
        Ok(_) => {
            let _ = writeln!(out, "removed worktree {}", plan.worktree.display());
        }
        Err(err) => {
            let _ = writeln!(out, "warning: could not remove the worktree: {err}");
        }
    }
    if canonical(&plan.worktree).exists() {
        let _ = writeln!(
            out,
            "warning: {} still exists; remove it by hand",
            plan.worktree.display()
        );
    }
    match run_git(repo, &["branch", "-d", &plan.branch]) {
        Ok(_) => {
            let _ = writeln!(out, "deleted branch {}", plan.branch);
        }
        Err(err) => {
            let _ = writeln!(
                out,
                "warning: could not delete branch {}: {err}",
                plan.branch
            );
        }
    }

    let mut state = UpstreamState::load_from(state_path);
    state.record_synced(&plan.tag);
    state
        .save_to(state_path)
        .map_err(|e| format!("applied, but failed to record the new base: {e}"))?;
    let _ = writeln!(out, "recorded upstream base {}", plan.tag);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::sync::test_support::{git, Fixture};
    use super::super::sync::{marker_path, prompt_path};
    use super::*;

    const TAG: &str = "v9.9.9";

    /// A fixture whose sync worktree has merged the tag, recorded the new base
    /// and written the verification marker, as a finished sync would.
    fn finished_sync(name: &str) -> (Fixture, PathBuf) {
        let fixture = Fixture::new(name);
        let repo = &fixture.repo;
        git(
            repo,
            &["remote", "add", "upstream", &fixture.upstream_url()],
        );
        git(repo, &["fetch", "upstream", "--tags"]);
        let worktree = worktree_path(repo, TAG);
        git(
            repo,
            &[
                "worktree",
                "add",
                "-b",
                &sync_branch_name(TAG),
                &worktree.to_string_lossy(),
                "flockr",
            ],
        );
        git(
            &worktree,
            &[
                "merge",
                "--no-ff",
                "--allow-unrelated-histories",
                "-m",
                "merge",
                TAG,
            ],
        );
        std::fs::write(worktree.join(BASE_FILE), format!("{TAG}\n")).unwrap();
        git(&worktree, &["add", "."]);
        git(&worktree, &["commit", "-q", "-m", "chore: sync upstream"]);
        std::fs::write(prompt_path(&worktree, TAG), "prompt").unwrap();
        let head = git(&worktree, &["rev-parse", "HEAD"]);
        std::fs::write(marker_path(&worktree, TAG), format!("{head}\n")).unwrap();
        (fixture, worktree)
    }

    fn state_file(fixture: &Fixture) -> PathBuf {
        fixture.root.join("state").join("upstream.json")
    }

    #[test]
    fn plan_describes_a_fast_forward_and_leaves_everything_untouched() {
        let (fixture, worktree) = finished_sync("apply-plan");
        let flockr_before = git(&fixture.repo, &["rev-parse", "flockr"]);
        let plan = plan_apply(&fixture.repo, TAG).unwrap();
        assert!(plan.fast_forward);
        assert_eq!(plan.flockr_head, flockr_before);
        assert_eq!(plan.worktree, worktree);
        assert_eq!(plan.branch, "upstream-sync/v9.9.9");

        let text = describe_plan(&plan).join("\n");
        assert!(text.contains("fast-forward `flockr`"));
        assert!(text.contains("cargo build --release"));
        assert!(text.contains("remove the sync worktree"));
        assert!(text.contains("reset --hard"));
        assert_eq!(git(&fixture.repo, &["rev-parse", "flockr"]), flockr_before);
    }

    #[test]
    fn apply_fast_forwards_builds_cleans_up_and_records_base() {
        let (fixture, worktree) = finished_sync("apply-ff");
        let plan = plan_apply(&fixture.repo, TAG).unwrap();
        let branch_head = plan.branch_head.clone();
        let state_path = state_file(&fixture);
        let state = UpstreamState {
            pending: vec![super::super::release::UpstreamRelease {
                tag: TAG.into(),
                name: String::new(),
                body: String::new(),
                prerelease: false,
                published_at: None,
                url: String::new(),
            }],
            ..Default::default()
        };
        state.save_to(&state_path).unwrap();

        let mut built_in = None;
        let mut out = Vec::new();
        execute_apply(
            &plan,
            &state_path,
            &mut |repo| {
                built_in = Some(repo.to_path_buf());
                Ok(true)
            },
            &mut out,
        )
        .unwrap();

        assert_eq!(built_in.as_deref(), Some(fixture.repo.as_path()));
        assert_eq!(git(&fixture.repo, &["rev-parse", "flockr"]), branch_head);
        assert!(
            fixture.repo.join("feature.txt").is_file(),
            "tag content merged"
        );
        assert!(git(&fixture.repo, &["status", "--porcelain"]).is_empty());
        assert!(!worktree.exists(), "worktree removed");
        assert!(git(&fixture.repo, &["branch", "--list", "upstream-sync/*"]).is_empty());
        let recorded = UpstreamState::load_from(&state_path);
        assert_eq!(recorded.synced_base.as_deref(), Some(TAG));
        assert!(recorded.pending.is_empty());
    }

    #[test]
    fn apply_creates_a_merge_commit_when_flockr_moved_on() {
        let (fixture, _worktree) = finished_sync("apply-merge");
        std::fs::write(fixture.repo.join("local.txt"), "flockr only\n").unwrap();
        git(&fixture.repo, &["add", "."]);
        git(&fixture.repo, &["commit", "-q", "-m", "feat: flockr only"]);

        let plan = plan_apply(&fixture.repo, TAG).unwrap();
        assert!(!plan.fast_forward);
        assert!(describe_plan(&plan).join("\n").contains("merge commit"));

        let mut out = Vec::new();
        execute_apply(&plan, &state_file(&fixture), &mut |_| Ok(true), &mut out).unwrap();
        let parents = git(
            &fixture.repo,
            &["rev-list", "--parents", "-n", "1", "flockr"],
        );
        assert_eq!(
            parents.split_whitespace().count(),
            3,
            "merge commit has two parents"
        );
        assert!(fixture.repo.join("local.txt").is_file());
        assert!(fixture.repo.join("feature.txt").is_file());
    }

    #[test]
    fn failed_build_keeps_merge_and_worktree_and_records_nothing() {
        let (fixture, worktree) = finished_sync("apply-buildfail");
        let plan = plan_apply(&fixture.repo, TAG).unwrap();
        let state_path = state_file(&fixture);
        let mut out = Vec::new();
        let err = execute_apply(&plan, &state_path, &mut |_| Ok(false), &mut out).unwrap_err();
        assert!(err.contains("reset --hard"), "{err}");
        assert!(err.contains(&plan.flockr_head), "{err}");
        assert!(worktree.exists());
        assert!(!state_path.exists());
        assert_eq!(
            git(&fixture.repo, &["rev-parse", "flockr"]),
            plan.branch_head,
            "the merge is kept so the user decides how to roll back"
        );
    }

    #[test]
    fn refuses_without_sync_branch_worktree_or_marker() {
        let fixture = Fixture::new("apply-nosync");
        let err = plan_apply(&fixture.repo, TAG).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");

        let (fixture, worktree) = finished_sync("apply-nomarker");
        std::fs::remove_file(marker_path(&worktree, TAG)).unwrap();
        let err = plan_apply(&fixture.repo, TAG).unwrap_err();
        assert!(err.contains("no recorded passing tests"), "{err}");
    }

    #[test]
    fn refuses_a_stale_marker_after_new_commits() {
        let (fixture, worktree) = finished_sync("apply-stale");
        std::fs::write(worktree.join("extra.txt"), "x").unwrap();
        git(&worktree, &["add", "."]);
        git(&worktree, &["commit", "-q", "-m", "fix: extra"]);
        let err = plan_apply(&fixture.repo, TAG).unwrap_err();
        assert!(
            err.contains("recorded passing tests are for commit"),
            "{err}"
        );
    }

    #[test]
    fn refuses_dirty_main_checkout_and_wrong_branch() {
        let (fixture, _worktree) = finished_sync("apply-dirty");
        std::fs::write(fixture.repo.join("README.md"), "edited\n").unwrap();
        let err = plan_apply(&fixture.repo, TAG).unwrap_err();
        assert!(
            err.contains("main checkout has uncommitted changes"),
            "{err}"
        );
        git(&fixture.repo, &["checkout", "--", "README.md"]);
        std::fs::write(fixture.repo.join("untracked.txt"), "u").unwrap();
        assert!(plan_apply(&fixture.repo, TAG)
            .unwrap_err()
            .contains("uncommitted changes"));
        std::fs::remove_file(fixture.repo.join("untracked.txt")).unwrap();

        git(&fixture.repo, &["checkout", "-q", "-b", "scratch"]);
        let err = plan_apply(&fixture.repo, TAG).unwrap_err();
        assert!(err.contains("is on `scratch`"), "{err}");
    }

    #[test]
    fn refuses_uncommitted_worktree_changes_but_allows_scratch_files() {
        let (fixture, worktree) = finished_sync("apply-wtdirty");
        // The prompt and marker are untracked and expected.
        assert!(plan_apply(&fixture.repo, TAG).is_ok());
        std::fs::write(worktree.join("herdr.txt"), "edited\n").unwrap();
        let err = plan_apply(&fixture.repo, TAG).unwrap_err();
        assert!(
            err.contains("sync worktree has uncommitted changes"),
            "{err}"
        );
    }

    #[test]
    fn refuses_when_upstream_base_was_not_updated() {
        let (fixture, worktree) = finished_sync("apply-base");
        std::fs::write(worktree.join(BASE_FILE), "v9.9.8\n").unwrap();
        git(&worktree, &["add", BASE_FILE]);
        git(&worktree, &["commit", "-q", "-m", "chore: revert base"]);
        let head = git(&worktree, &["rev-parse", "HEAD"]);
        std::fs::write(marker_path(&worktree, TAG), format!("{head}\n")).unwrap();
        let err = plan_apply(&fixture.repo, TAG).unwrap_err();
        assert!(err.contains("upstream-base"), "{err}");
    }

    #[test]
    fn refuses_when_the_tag_is_not_merged_into_the_branch() {
        let fixture = Fixture::new("apply-unmerged");
        let repo = &fixture.repo;
        git(
            repo,
            &["remote", "add", "upstream", &fixture.upstream_url()],
        );
        git(repo, &["fetch", "upstream", "--tags"]);
        let worktree = worktree_path(repo, TAG);
        git(
            repo,
            &[
                "worktree",
                "add",
                "-b",
                &sync_branch_name(TAG),
                &worktree.to_string_lossy(),
                "flockr",
            ],
        );
        std::fs::write(worktree.join(BASE_FILE), format!("{TAG}\n")).unwrap();
        git(&worktree, &["add", "."]);
        git(&worktree, &["commit", "-q", "-m", "chore: base only"]);
        let head = git(&worktree, &["rev-parse", "HEAD"]);
        std::fs::write(marker_path(&worktree, TAG), format!("{head}\n")).unwrap();
        let err = plan_apply(repo, TAG).unwrap_err();
        assert!(err.contains("is not merged into"), "{err}");
    }

    #[test]
    fn scratch_file_detection_matches_only_the_tag_files() {
        assert!(is_sync_scratch_file("?? .flockr/sync-v1.0.0.md", "v1.0.0"));
        assert!(is_sync_scratch_file(
            "?? .flockr/sync-v1.0.0.verified",
            "v1.0.0"
        ));
        assert!(!is_sync_scratch_file("?? .flockr/sync-v2.0.0.md", "v1.0.0"));
        assert!(!is_sync_scratch_file(" M src/main.rs", "v1.0.0"));
        assert!(!is_sync_scratch_file(
            "?? .flockr/parity-v1.0.0.md",
            "v1.0.0"
        ));
    }
}
