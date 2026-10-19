//! Applies upstream herdr's changes between two release tags onto a worktree
//! that shares no git history with herdr.
//!
//! flockr's history starts at a root commit that holds herdr's tree without a
//! parent link, so `git merge <tag>` refuses ("unrelated histories"). Instead
//! this module replays upstream's diff `<base>..<tag>` file by file as a
//! three-way merge: base = the file at the base tag, theirs = the file at the
//! target tag, ours = the file in the worktree. The base blobs exist locally
//! after `git fetch upstream --tags`, so no common ancestor commit is needed.
//!
//! Why not `git diff | git apply --3way` or `git format-patch | git am -3`:
//! `git apply --3way` aborts the whole patch when one path is missing from the
//! index (flockr deleted upstream's CI workflows, so this happens on a real
//! release), cannot follow a file flockr renamed (`herdr` -> `flockr` paths)
//! without rewriting patch headers, and has no per-file report. `git am -3`
//! stops at the first commit that conflicts and shares those two limits. Doing
//! the merge per file with `git merge-file` has none of them: every path is
//! looked up through a rename table first, missing files are reported instead
//! of aborting, binaries and mode changes are handled explicitly, and the
//! result is one outcome per file. Upstream's commit messages are not kept;
//! `git log <base>..<tag>` still shows them.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Prefix of the closing marker `git merge-file` writes for a conflict. The
/// label after it is `herdr <tag>`. `flockr upstream apply` refuses a branch
/// that still contains a line starting with it.
pub(crate) const CONFLICT_MARKER_PREFIX: &str = ">>>>>>> herdr ";

/// Minimum similarity for a rename to enter the table.
const RENAME_THRESHOLD: &str = "-M40%";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Applied without conflict markers (staged in the index).
    Applied,
    /// Conflict markers were written, or the file needs a manual decision.
    Conflict,
    /// Not applied because flockr has no such file; needs a look.
    Skipped,
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    /// Path in upstream herdr (new path for an upstream rename).
    pub upstream_path: String,
    /// Path in flockr that was touched.
    pub flockr_path: String,
    pub outcome: Outcome,
    pub note: String,
}

#[derive(Debug, Default)]
pub(crate) struct PatchReport {
    pub base_tag: String,
    pub tag: String,
    pub entries: Vec<Entry>,
    /// Entries whose flockr path differs from the upstream path.
    pub renamed: usize,
}

impl PatchReport {
    pub(crate) fn count(&self, outcome: Outcome) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.outcome == outcome)
            .count()
    }

    pub(crate) fn one_line(&self) -> String {
        format!(
            "upstream {}..{}: {} files changed; {} applied cleanly, {} conflicted, {} skipped ({} mapped through the rename table)",
            self.base_tag,
            self.tag,
            self.entries.len(),
            self.count(Outcome::Applied),
            self.count(Outcome::Conflict),
            self.count(Outcome::Skipped),
            self.renamed
        )
    }

    /// The prompt section describing the result. It always starts with the
    /// heading `## Upstream patch result`.
    pub(crate) fn markdown(&self) -> String {
        let mut text = format!("{PATCH_SECTION_HEADING}\n\n{}.\n\n", self.one_line());
        let describe = |entry: &Entry| {
            if entry.upstream_path == entry.flockr_path {
                format!("- `{}`: {}\n", entry.flockr_path, entry.note)
            } else {
                format!(
                    "- `{}` (upstream `{}`): {}\n",
                    entry.flockr_path, entry.upstream_path, entry.note
                )
            }
        };
        let conflicts: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.outcome == Outcome::Conflict)
            .collect();
        text.push_str(&format!(
            "### Conflicts: resolve these first ({})\n\n",
            conflicts.len()
        ));
        if conflicts.is_empty() {
            text.push_str("None.\n");
        }
        for entry in conflicts {
            text.push_str(&describe(entry));
        }
        let skipped: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.outcome == Outcome::Skipped)
            .collect();
        text.push_str(&format!(
            "\n### Skipped: flockr has no such file ({})\n\n",
            skipped.len()
        ));
        if skipped.is_empty() {
            text.push_str("None.\n");
        }
        for entry in skipped {
            text.push_str(&describe(entry));
        }
        let applied: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.outcome == Outcome::Applied)
            .collect();
        text.push_str(&format!(
            "\n### Applied cleanly and staged ({})\n\n",
            applied.len()
        ));
        const LISTED: usize = 80;
        for entry in applied.iter().take(LISTED) {
            text.push_str(&describe(entry));
        }
        if applied.len() > LISTED {
            text.push_str(&format!(
                "- ... and {} more (see `git diff --cached --stat`)\n",
                applied.len() - LISTED
            ));
        }
        text.push('\n');
        text
    }
}

/// Heading of the prompt section; used to carry the result over when a sync is
/// resumed.
pub(crate) const PATCH_SECTION_HEADING: &str = "## Upstream patch result";

/// One line of `git diff --raw -z`.
#[derive(Debug, Clone)]
struct RawEntry {
    old_mode: String,
    new_mode: String,
    old_sha: String,
    new_sha: String,
    status: char,
    /// Old path (and, for renames, the new path in `path`).
    old_path: String,
    path: String,
}

fn null_sha(sha: &str) -> bool {
    sha.bytes().all(|b| b == b'0')
}

fn parse_raw(output: &[u8]) -> Result<Vec<RawEntry>, String> {
    let mut parts = output
        .split(|b| *b == 0)
        .map(|p| String::from_utf8_lossy(p));
    let mut entries = Vec::new();
    while let Some(header) = parts.next() {
        if header.is_empty() {
            continue;
        }
        let header = header.trim_start_matches(':').to_string();
        let fields: Vec<&str> = header.split(' ').collect();
        if fields.len() != 5 {
            return Err(format!("unexpected git diff --raw record {header:?}"));
        }
        let status = fields[4].chars().next().unwrap_or('?');
        let first = parts
            .next()
            .ok_or_else(|| "truncated git diff --raw output".to_string())?
            .into_owned();
        let (old_path, path) = if matches!(status, 'R' | 'C') {
            let second = parts
                .next()
                .ok_or_else(|| "truncated git diff --raw output".to_string())?
                .into_owned();
            (first, second)
        } else {
            (first.clone(), first)
        };
        entries.push(RawEntry {
            old_mode: fields[0].to_string(),
            new_mode: fields[1].to_string(),
            old_sha: fields[2].to_string(),
            new_sha: fields[3].to_string(),
            status,
            old_path,
            path,
        });
    }
    Ok(entries)
}

/// Maps upstream paths to the paths flockr uses.
#[derive(Debug, Default)]
struct RenameMap {
    files: HashMap<String, String>,
    /// Directory moves inferred from renames that keep the file name.
    dirs: Vec<(String, String)>,
}

fn split_dir(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

impl RenameMap {
    fn from_entries(entries: &[RawEntry]) -> Self {
        let mut map = Self::default();
        let mut dirs: HashMap<String, String> = HashMap::new();
        for entry in entries.iter().filter(|entry| entry.status == 'R') {
            map.files.insert(entry.old_path.clone(), entry.path.clone());
            let (old_dir, old_name) = split_dir(&entry.old_path);
            let (new_dir, new_name) = split_dir(&entry.path);
            if old_name == new_name && old_dir != new_dir && !old_dir.is_empty() {
                dirs.entry(old_dir.to_string())
                    .or_insert_with(|| new_dir.to_string());
            }
        }
        map.dirs = dirs.into_iter().collect();
        // Longest prefix first.
        map.dirs
            .sort_by_key(|(old, _)| std::cmp::Reverse(old.len()));
        map
    }

    /// `exists` tells whether flockr has the path unchanged.
    fn resolve(&self, path: &str, exists: &dyn Fn(&str) -> bool) -> String {
        if let Some(mapped) = self.files.get(path) {
            return mapped.clone();
        }
        if exists(path) {
            return path.to_string();
        }
        for (old, new) in &self.dirs {
            if let Some(rest) = path.strip_prefix(old.as_str()) {
                if let Some(rest) = rest.strip_prefix('/') {
                    return format!("{new}/{rest}");
                }
            }
        }
        path.to_string()
    }
}

struct Git<'a> {
    worktree: &'a Path,
}

struct GitOutput {
    code: i32,
    stdout: Vec<u8>,
    stderr: String,
}

impl Git<'_> {
    fn run(&self, args: &[&str]) -> Result<GitOutput, String> {
        let output = crate::noninteractive_process::command("git")
            .arg("-C")
            .arg(self.worktree)
            .arg("--literal-pathspecs")
            .args(["-c", "core.quotepath=off", "-c", "core.autocrlf=false"])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| format!("failed to run git: {e}"))?;
        Ok(GitOutput {
            code: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }

    fn ok(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let output = self.run(args)?;
        if output.code == 0 {
            Ok(output.stdout)
        } else {
            Err(format!("git {} failed: {}", args.join(" "), output.stderr))
        }
    }

    /// `(mode, sha)` of a blob at `path` in `HEAD`.
    fn head_blob(&self, path: &str) -> Option<(String, String)> {
        let output = self.run(&["ls-tree", "HEAD", "--", path]).ok()?;
        if output.code != 0 {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let line = text.lines().next()?;
        let (meta, _) = line.split_once('\t')?;
        let mut fields = meta.split(' ');
        let mode = fields.next()?;
        let kind = fields.next()?;
        let sha = fields.next()?;
        (kind == "blob").then(|| (mode.to_string(), sha.to_string()))
    }

    fn blob_bytes(&self, sha: &str) -> Result<Vec<u8>, String> {
        self.ok(&["cat-file", "blob", sha])
    }

    /// Writes the blob into the index and the working tree with `mode`.
    fn take_theirs(&self, path: &str, mode: &str, sha: &str) -> Result<(), String> {
        self.ok(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("{mode},{sha},{path}"),
        ])?;
        self.ok(&["checkout-index", "-f", "--", path])?;
        Ok(())
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!(
            "flockr-upstream-patch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create scratch dir: {e}"))?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

enum Merge {
    Clean,
    Conflicts(i32),
    Binary,
}

struct Applier<'a> {
    git: Git<'a>,
    worktree: &'a Path,
    scratch: Scratch,
    tag: &'a str,
    base_tag: &'a str,
}

impl Applier<'_> {
    /// Three-way merges `theirs` into the working file `work_path`. `base` is
    /// `None` for an add/add collision (empty base).
    fn merge_into(
        &self,
        work_path: &str,
        base: Option<&str>,
        theirs: &str,
    ) -> Result<Merge, String> {
        let ours_file = self.worktree.join(work_path);
        let ours =
            std::fs::read(&ours_file).map_err(|e| format!("cannot read {work_path}: {e}"))?;
        let base_bytes = match base {
            Some(sha) => self.git.blob_bytes(sha)?,
            None => Vec::new(),
        };
        let theirs_bytes = self.git.blob_bytes(theirs)?;
        if is_binary(&ours) || is_binary(&base_bytes) || is_binary(&theirs_bytes) {
            return Ok(Merge::Binary);
        }
        let base_file = self.scratch.0.join("base");
        let theirs_file = self.scratch.0.join("theirs");
        std::fs::write(&base_file, &base_bytes).map_err(|e| e.to_string())?;
        std::fs::write(&theirs_file, &theirs_bytes).map_err(|e| e.to_string())?;
        let base_label = format!("herdr {}", self.base_tag);
        let theirs_label = format!("herdr {}", self.tag);
        let ours_arg = ours_file.to_string_lossy().into_owned();
        let base_arg = base_file.to_string_lossy().into_owned();
        let theirs_arg = theirs_file.to_string_lossy().into_owned();
        let output = self.git.run(&[
            "merge-file",
            "-L",
            "flockr",
            "-L",
            &base_label,
            "-L",
            &theirs_label,
            &ours_arg,
            &base_arg,
            &theirs_arg,
        ])?;
        match output.code {
            0 => Ok(Merge::Clean),
            n if (1..=127).contains(&n) => Ok(Merge::Conflicts(n)),
            _ => Err(format!("git merge-file failed: {}", output.stderr)),
        }
    }

    fn merge_outcome(
        &self,
        work_path: &str,
        merge: Merge,
        mode_change: Option<&str>,
        clean_note: &str,
    ) -> Result<(Outcome, String), String> {
        match merge {
            Merge::Clean => {
                self.git.ok(&["add", "--", work_path])?;
                if let Some(flag) = mode_change {
                    self.git
                        .ok(&["update-index", flag, "--", work_path])?;
                    set_exec_bit(&self.worktree.join(work_path), flag == "--chmod=+x");
                }
                Ok((Outcome::Applied, clean_note.to_string()))
            }
            Merge::Conflicts(n) => Ok((
                Outcome::Conflict,
                format!("{n} conflicting hunk(s); markers written in the file"),
            )),
            Merge::Binary => Ok((
                Outcome::Conflict,
                "binary file changed in both; flockr's version kept, take or reject upstream's by hand"
                    .to_string(),
            )),
        }
    }

    fn add(
        &self,
        entry: &RawEntry,
        dst: &str,
        prefix_note: &str,
    ) -> Result<(Outcome, String), String> {
        if entry.new_mode == "160000" {
            return Ok((
                Outcome::Skipped,
                "upstream added a submodule; not handled".to_string(),
            ));
        }
        match self.git.head_blob(dst) {
            None => {
                self.git.take_theirs(dst, &entry.new_mode, &entry.new_sha)?;
                Ok((
                    Outcome::Applied,
                    format!("{prefix_note}added from upstream"),
                ))
            }
            Some((_, sha)) if sha == entry.new_sha => {
                Ok((Outcome::Applied, "already identical in flockr".to_string()))
            }
            Some(_) => {
                let merge = self.merge_into(dst, None, &entry.new_sha)?;
                let (outcome, note) = self.merge_outcome(dst, merge, None, "")?;
                Ok((
                    outcome,
                    format!("{prefix_note}added upstream but flockr has its own file: {note}"),
                ))
            }
        }
    }

    fn delete(&self, entry: &RawEntry, dst: &str) -> Result<(Outcome, String), String> {
        match self.git.head_blob(dst) {
            None => Ok((
                Outcome::Skipped,
                "deleted upstream; flockr already has no such file".to_string(),
            )),
            Some((_, sha)) if sha == entry.old_sha => {
                self.git.ok(&["rm", "-q", "-f", "--", dst])?;
                Ok((Outcome::Applied, "deleted (as upstream)".to_string()))
            }
            Some(_) => Ok((
                Outcome::Conflict,
                "deleted upstream but flockr modified it; file kept, decide by hand".to_string(),
            )),
        }
    }

    /// Modification; `head_path` is where flockr's version lives in `HEAD`,
    /// `work_path` where it is now in the worktree (they differ after a
    /// rename that was already performed).
    fn modify(
        &self,
        entry: &RawEntry,
        head_path: &str,
        work_path: &str,
    ) -> Result<(Outcome, String), String> {
        let Some((ours_mode, ours_sha)) = self.git.head_blob(head_path) else {
            return Ok((
                Outcome::Skipped,
                "changed upstream but flockr has no such file (removed or moved); review whether it still applies"
                    .to_string(),
            ));
        };
        if ours_sha == entry.new_sha && ours_mode == entry.new_mode {
            return Ok((Outcome::Applied, "already identical in flockr".to_string()));
        }
        let mode_follows = ours_mode == entry.old_mode;
        if ours_sha == entry.old_sha {
            let mode = if mode_follows {
                &entry.new_mode
            } else {
                &ours_mode
            };
            self.git.take_theirs(work_path, mode, &entry.new_sha)?;
            return Ok((
                Outcome::Applied,
                "flockr never changed this file; copied upstream's version".to_string(),
            ));
        }
        if [&ours_mode, &entry.new_mode, &entry.old_mode]
            .iter()
            .any(|mode| mode.as_str() == "120000" || mode.as_str() == "160000")
        {
            return Ok((
                Outcome::Conflict,
                "symlink or submodule changed upstream and differs in flockr; decide by hand"
                    .to_string(),
            ));
        }
        let mode_change = if mode_follows && entry.old_mode != entry.new_mode {
            match entry.new_mode.as_str() {
                "100755" => Some("--chmod=+x"),
                "100644" => Some("--chmod=-x"),
                _ => None,
            }
        } else {
            None
        };
        let merge = self.merge_into(work_path, Some(&entry.old_sha), &entry.new_sha)?;
        self.merge_outcome(work_path, merge, mode_change, "merged cleanly (3-way)")
    }

    fn rename(
        &self,
        entry: &RawEntry,
        dst_old: &str,
        dst_new: &str,
    ) -> Result<(Outcome, String), String> {
        if self.git.head_blob(dst_old).is_none() {
            let (outcome, note) = self.add(
                entry,
                dst_new,
                &format!(
                    "renamed upstream from `{}`, old path absent in flockr: ",
                    entry.old_path
                ),
            )?;
            return Ok((outcome, note));
        }
        if dst_old != dst_new {
            if self.git.head_blob(dst_new).is_some() || self.worktree.join(dst_new).exists() {
                return Ok((
                    Outcome::Conflict,
                    format!(
                        "renamed upstream from `{}` but `{dst_new}` already exists in flockr; merge by hand",
                        entry.old_path
                    ),
                ));
            }
            if let Some(parent) = Path::new(dst_new).parent() {
                std::fs::create_dir_all(self.worktree.join(parent)).map_err(|e| e.to_string())?;
            }
            self.git.ok(&["mv", "--", dst_old, dst_new])?;
        }
        let (outcome, note) = self.modify(entry, dst_old, dst_new)?;
        Ok((
            outcome,
            format!("renamed upstream from `{}`; {note}", entry.old_path),
        ))
    }
}

#[cfg(unix)]
fn set_exec_bit(path: &Path, executable: bool) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        let mode = perms.mode();
        perms.set_mode(if executable {
            mode | 0o111
        } else {
            mode & !0o111
        });
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_exec_bit(_path: &Path, _executable: bool) {}

/// Applies upstream's `base_tag..tag` changes to the worktree (whose `HEAD`
/// is the flockr branch). Clean results are staged; conflicts stay in the
/// working tree with markers. Nothing is committed.
pub(crate) fn apply_upstream_range(
    worktree: &Path,
    base_tag: &str,
    tag: &str,
    out: &mut dyn Write,
) -> Result<PatchReport, String> {
    let git = Git { worktree };
    let base_ref = format!("refs/tags/{base_tag}^{{commit}}");
    let tag_ref = format!("refs/tags/{tag}^{{commit}}");
    git.ok(&["rev-parse", "--verify", "--quiet", &base_ref])
        .map_err(|_| {
            format!("the recorded base tag {base_tag} is not available locally; fetch it with `git fetch upstream --tags`")
        })?;
    git.ok(&["rev-parse", "--verify", "--quiet", &tag_ref])
        .map_err(|_| format!("tag {tag} is not available locally"))?;
    let base_commit = String::from_utf8_lossy(&git.ok(&["rev-parse", &base_ref])?)
        .trim()
        .to_string();
    let tag_commit = String::from_utf8_lossy(&git.ok(&["rev-parse", &tag_ref])?)
        .trim()
        .to_string();

    let rename_limit = "diff.renameLimit=20000";
    let flockr_changes = parse_raw(&git.ok(&[
        "-c",
        rename_limit,
        "diff",
        "--raw",
        "-z",
        "--no-abbrev",
        RENAME_THRESHOLD,
        &base_commit,
        "HEAD",
    ])?)?;
    let renames = RenameMap::from_entries(&flockr_changes);
    let _ = writeln!(
        out,
        "rename table: {} renamed files, {} directory moves (flockr vs {base_tag})",
        renames.files.len(),
        renames.dirs.len()
    );

    let upstream_changes = parse_raw(&git.ok(&[
        "-c",
        rename_limit,
        "diff",
        "--raw",
        "-z",
        "--no-abbrev",
        RENAME_THRESHOLD,
        &base_commit,
        &tag_commit,
    ])?)?;

    let applier = Applier {
        git: Git { worktree },
        worktree,
        scratch: Scratch::new()?,
        tag,
        base_tag,
    };
    let exists = |path: &str| applier.git.head_blob(path).is_some();
    let mut report = PatchReport {
        base_tag: base_tag.to_string(),
        tag: tag.to_string(),
        ..Default::default()
    };
    let mut touched: HashSet<String> = HashSet::new();
    for entry in &upstream_changes {
        if null_sha(&entry.new_sha) && entry.status != 'D' {
            continue;
        }
        let upstream_path = entry.path.clone();
        let dst_new = renames.resolve(&entry.path, &exists);
        let dst_old = renames.resolve(&entry.old_path, &exists);
        let flockr_path = if entry.status == 'D' {
            dst_old.clone()
        } else {
            dst_new.clone()
        };
        if !touched.insert(flockr_path.clone()) {
            report.entries.push(Entry {
                upstream_path,
                flockr_path,
                outcome: Outcome::Conflict,
                note: "several upstream changes map to this path; apply the remaining one by hand"
                    .to_string(),
            });
            continue;
        }
        let result = match entry.status {
            'A' | 'C' => applier.add(entry, &dst_new, ""),
            'D' => applier.delete(entry, &dst_old),
            'M' | 'T' => applier.modify(entry, &dst_new, &dst_new),
            'R' => applier.rename(entry, &dst_old, &dst_new),
            other => Ok((
                Outcome::Conflict,
                format!("unsupported change status {other}; apply by hand"),
            )),
        };
        let (outcome, note) = result.unwrap_or_else(|err| {
            (
                Outcome::Conflict,
                format!("could not be applied automatically: {err}"),
            )
        });
        if flockr_path != upstream_path || (entry.status == 'R' && dst_old != entry.old_path) {
            report.renamed += 1;
        }
        report.entries.push(Entry {
            upstream_path,
            flockr_path,
            outcome,
            note,
        });
    }
    report
        .entries
        .sort_by(|a, b| a.flockr_path.cmp(&b.flockr_path));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::sync::test_support::{git, unique_dir};
    use super::*;

    /// Orphan flockr history holding upstream v1's tree (modified), plus an
    /// upstream repo with v1 and v2. Returns (root, upstream, flockr).
    struct Pair {
        root: PathBuf,
        flockr: PathBuf,
    }

    impl Drop for Pair {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn write(dir: &Path, path: &str, content: &str) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }

    fn lines(prefix: &str, n: usize) -> String {
        (1..=n).map(|i| format!("{prefix} line {i}\n")).collect()
    }

    fn pair(name: &str, flockr_edit: impl FnOnce(&Path), upstream_v2: impl FnOnce(&Path)) -> Pair {
        let root = unique_dir(name);
        let upstream = root.join("upstream");
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["init", "-q", "-b", "main"]);
        write(&upstream, "src/lib.rs", &lines("lib", 30));
        write(&upstream, "src/herdr_thing.rs", &lines("thing", 30));
        write(&upstream, "src/gone.rs", &lines("gone", 10));
        write(&upstream, "docs/guide.md", &lines("guide", 10));
        write(&upstream, "ci/old.yml", "ci\n");
        write(&upstream, "tool.sh", &lines("tool", 8));
        std::fs::write(upstream.join("logo.bin"), [0u8, 1, 2, 3, 4, 5]).unwrap();
        git(&upstream, &["add", "."]);
        git(&upstream, &["commit", "-q", "-m", "v1"]);
        git(&upstream, &["tag", "v1.0.0"]);
        upstream_v2(&upstream);
        git(&upstream, &["add", "-A"]);
        if upstream.join("run.sh").exists() {
            // `run.sh` is an executable script upstream (mode change by git).
            git(&upstream, &["update-index", "--chmod=+x", "run.sh"]);
        }
        git(&upstream, &["commit", "-q", "-m", "v2"]);
        git(&upstream, &["tag", "v1.1.0"]);

        // flockr: unrelated history whose first commit is the v1 tree.
        let flockr = root.join("flockr");
        std::fs::create_dir_all(&flockr).unwrap();
        git(&flockr, &["init", "-q", "-b", "flockr"]);
        git(
            &flockr,
            &["fetch", "-q", upstream.to_str().unwrap(), "--tags"],
        );
        git(&flockr, &["checkout", "-q", "--orphan", "tmp", "v1.0.0"]);
        write(&flockr, ".flockr/upstream-base", "v1.0.0\n");
        git(&flockr, &["add", "."]);
        git(&flockr, &["commit", "-q", "-m", "flockr base"]);
        git(&flockr, &["branch", "-M", "flockr"]);
        flockr_edit(&flockr);
        git(&flockr, &["add", "-A"]);
        git(
            &flockr,
            &["commit", "-q", "--allow-empty", "-m", "flockr changes"],
        );
        Pair { root, flockr }
    }

    fn run(flockr: &Path) -> (PatchReport, String) {
        let mut out = Vec::new();
        let report = apply_upstream_range(flockr, "v1.0.0", "v1.1.0", &mut out).unwrap();
        (report, String::from_utf8(out).unwrap())
    }

    fn entry<'a>(report: &'a PatchReport, flockr_path: &str) -> &'a Entry {
        report
            .entries
            .iter()
            .find(|e| e.flockr_path == flockr_path)
            .unwrap_or_else(|| panic!("no entry for {flockr_path}: {:?}", report.entries))
    }

    #[test]
    fn applies_cleanly_conflicts_and_maps_renamed_paths_without_shared_history() {
        let pair = pair(
            "patch-main",
            |flockr| {
                // flockr renamed herdr_thing.rs and edited its first line.
                git(flockr, &["mv", "src/herdr_thing.rs", "src/flockr_thing.rs"]);
                let mut text = std::fs::read_to_string(flockr.join("src/flockr_thing.rs")).unwrap();
                text = text.replacen("thing line 1\n", "flockr thing line 1\n", 1);
                std::fs::write(flockr.join("src/flockr_thing.rs"), text).unwrap();
                // flockr edited lib.rs line 15 (conflicts) and removed gone.rs.
                let lib = std::fs::read_to_string(flockr.join("src/lib.rs"))
                    .unwrap()
                    .replace("lib line 15\n", "flockr line 15\n");
                std::fs::write(flockr.join("src/lib.rs"), lib).unwrap();
                git(flockr, &["rm", "-q", "src/gone.rs"]);
                // flockr deleted CI and edited the guide's first line.
                git(flockr, &["rm", "-q", "ci/old.yml"]);
                let guide = std::fs::read_to_string(flockr.join("docs/guide.md"))
                    .unwrap()
                    .replacen("guide line 1\n", "flockr guide\n", 1);
                std::fs::write(flockr.join("docs/guide.md"), guide).unwrap();
            },
            |upstream| {
                let lib = std::fs::read_to_string(upstream.join("src/lib.rs"))
                    .unwrap()
                    .replace("lib line 15\n", "upstream line 15\n");
                std::fs::write(upstream.join("src/lib.rs"), lib).unwrap();
                let thing = std::fs::read_to_string(upstream.join("src/herdr_thing.rs"))
                    .unwrap()
                    .replace("thing line 30\n", "thing line 30 changed\n");
                std::fs::write(upstream.join("src/herdr_thing.rs"), thing).unwrap();
                write(upstream, "src/gone.rs", &lines("gone", 11));
                write(upstream, "ci/old.yml", "ci v2\n");
                let guide = std::fs::read_to_string(upstream.join("docs/guide.md"))
                    .unwrap()
                    .replace("guide line 10\n", "guide line 10 new\n");
                std::fs::write(upstream.join("docs/guide.md"), guide).unwrap();
                write(upstream, "src/new_file.rs", "brand new\n");
                std::fs::write(upstream.join("logo.bin"), [9u8, 8, 7, 0]).unwrap();
                git(upstream, &["rm", "-q", "tool.sh"]);
                write(upstream, "run.sh", &lines("run", 8));
            },
        );
        let flockr_before = git(&pair.flockr, &["rev-parse", "flockr"]);
        let wt = pair.root.join("wt");
        git(
            &pair.flockr,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "sync",
                wt.to_str().unwrap(),
                "flockr",
            ],
        );

        // Sanity: the histories really are unrelated, so a merge cannot work.
        assert!(!super::super::sync::git_succeeds(
            &wt,
            &["merge", "--no-commit", "--no-ff", "v1.1.0"]
        ));

        let (report, out) = run(&wt);
        assert!(out.contains("rename table: 1 renamed files"), "{out}");

        // Renamed path receives upstream's change through the rename table.
        let thing = entry(&report, "src/flockr_thing.rs");
        assert_eq!(thing.outcome, Outcome::Applied, "{thing:?}");
        assert_eq!(thing.upstream_path, "src/herdr_thing.rs");
        let text = std::fs::read_to_string(wt.join("src/flockr_thing.rs")).unwrap();
        assert!(text.contains("thing line 30 changed") && text.contains("flockr thing line 1"));
        assert!(!wt.join("src/herdr_thing.rs").exists());

        // Conflicting edit: markers in the file, not staged.
        let lib = entry(&report, "src/lib.rs");
        assert_eq!(lib.outcome, Outcome::Conflict, "{lib:?}");
        let text = std::fs::read_to_string(wt.join("src/lib.rs")).unwrap();
        assert!(text.contains("<<<<<<< flockr"), "{text}");
        assert!(text.contains(">>>>>>> herdr v1.1.0"), "{text}");
        assert!(text.contains("flockr line 15") && text.contains("upstream line 15"));

        // Non-overlapping edit merges cleanly with flockr's own change kept.
        let guide = entry(&report, "docs/guide.md");
        assert_eq!(guide.outcome, Outcome::Applied, "{guide:?}");
        let text = std::fs::read_to_string(wt.join("docs/guide.md")).unwrap();
        assert!(text.contains("flockr guide") && text.contains("guide line 10 new"));

        // Files flockr removed are skipped, never resurrected.
        for gone in ["src/gone.rs", "ci/old.yml"] {
            assert_eq!(entry(&report, gone).outcome, Outcome::Skipped, "{gone}");
            assert!(!wt.join(gone).exists(), "{gone} must stay deleted");
        }

        // New file, binary change and delete + exec-bit add.
        assert_eq!(entry(&report, "src/new_file.rs").outcome, Outcome::Applied);
        assert_eq!(
            std::fs::read(wt.join("logo.bin")).unwrap(),
            vec![9u8, 8, 7, 0]
        );
        assert_eq!(entry(&report, "tool.sh").outcome, Outcome::Applied);
        assert!(!wt.join("tool.sh").exists());
        assert_eq!(entry(&report, "run.sh").outcome, Outcome::Applied);
        let staged = git(&wt, &["ls-files", "-s", "run.sh"]);
        assert!(staged.starts_with("100755"), "{staged}");

        assert_eq!(report.count(Outcome::Conflict), 1, "{:?}", report.entries);
        assert_eq!(report.count(Outcome::Skipped), 2);
        assert!(report.renamed >= 1);
        let markdown = report.markdown();
        assert!(markdown.starts_with("## Upstream patch result"));
        assert!(markdown.contains("`src/flockr_thing.rs` (upstream `src/herdr_thing.rs`)"));
        assert!(markdown.contains("Conflicts: resolve these first (1)"));

        // The flockr branch itself is untouched.
        assert_eq!(git(&pair.flockr, &["rev-parse", "flockr"]), flockr_before);
        assert!(git(&pair.flockr, &["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn upstream_rename_follows_flockrs_edited_file_and_binary_conflicts_are_kept() {
        let pair = pair(
            "patch-rename",
            |flockr| {
                std::fs::write(flockr.join("logo.bin"), [5u8, 5, 5, 0]).unwrap();
                let guide = std::fs::read_to_string(flockr.join("docs/guide.md"))
                    .unwrap()
                    .replacen("guide line 1\n", "flockr guide\n", 1);
                std::fs::write(flockr.join("docs/guide.md"), guide).unwrap();
            },
            |upstream| {
                std::fs::write(upstream.join("logo.bin"), [9u8, 9, 9, 0]).unwrap();
                git(upstream, &["mv", "docs/guide.md", "docs/handbook.md"]);
            },
        );
        let wt = pair.root.join("wt");
        git(
            &pair.flockr,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "sync",
                wt.to_str().unwrap(),
                "flockr",
            ],
        );
        let (report, _) = run(&wt);
        let logo = entry(&report, "logo.bin");
        assert_eq!(logo.outcome, Outcome::Conflict, "{logo:?}");
        assert_eq!(
            std::fs::read(wt.join("logo.bin")).unwrap(),
            vec![5u8, 5, 5, 0]
        );
        let handbook = entry(&report, "docs/handbook.md");
        assert_eq!(handbook.outcome, Outcome::Applied, "{handbook:?}");
        assert!(std::fs::read_to_string(wt.join("docs/handbook.md"))
            .unwrap()
            .contains("flockr guide"));
        assert!(!wt.join("docs/guide.md").exists());
    }

    #[test]
    fn rename_table_follows_directory_moves_for_new_upstream_files() {
        let pair = pair(
            "patch-dirs",
            |flockr| {
                std::fs::create_dir_all(flockr.join("manual")).unwrap();
                git(flockr, &["mv", "docs/guide.md", "manual/guide.md"]);
            },
            |upstream| write(upstream, "docs/extra.md", "extra\n"),
        );
        let wt = pair.root.join("wt");
        git(
            &pair.flockr,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "sync",
                wt.to_str().unwrap(),
                "flockr",
            ],
        );
        let (report, _) = run(&wt);
        assert_eq!(entry(&report, "manual/extra.md").outcome, Outcome::Applied);
        assert_eq!(
            entry(&report, "manual/extra.md").upstream_path,
            "docs/extra.md"
        );
    }

    #[test]
    fn missing_base_tag_is_reported() {
        let pair = pair(
            "patch-nobase",
            |_| {},
            |upstream| write(upstream, "x", "x\n"),
        );
        let mut out = Vec::new();
        let err = apply_upstream_range(&pair.flockr, "v0.0.1", "v1.1.0", &mut out).unwrap_err();
        assert!(err.contains("v0.0.1"), "{err}");
    }
}
