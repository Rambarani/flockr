#![cfg(unix)]
//! `flockr job new|validate|run --dry-run|schema|skill` end to end through the
//! real binary, with HOME and the XDG directories pointed at a scratch
//! directory. None of these commands needs a server.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Home {
    root: PathBuf,
}

impl Home {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "flockr-job-cli-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        Self { root }
    }

    fn config(&self) -> PathBuf {
        self.root.join("config")
    }

    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    fn jobs(&self) -> PathBuf {
        // Debug builds use `flockr-dev`, release builds `flockr`.
        let app = if cfg!(debug_assertions) {
            "flockr-dev"
        } else {
            "flockr"
        };
        self.config().join(app).join("jobs")
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_flockr"));
        command.args(args);
        for (name, value) in [
            ("HOME", self.root.join("home")),
            ("XDG_CONFIG_HOME", self.config()),
            ("XDG_STATE_HOME", self.state()),
            ("XDG_RUNTIME_DIR", self.root.join("run")),
        ] {
            command.env(name, value);
        }
        command.env("PATH", "/usr/bin:/bin");
        for name in [
            "SHELL",
            "FLOCKR_ENV",
            "FLOCKR_SOCKET_PATH",
            "FLOCKR_CLIENT_SOCKET_PATH",
        ] {
            command.env_remove(name);
        }
        command
            .current_dir(&self.root)
            .output()
            .expect("run flockr")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn listing(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            out.push(entry.path().display().to_string());
            if entry.path().is_dir() {
                walk(&entry.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

#[test]
fn new_validate_and_dry_run_work_without_a_server_and_record_nothing() {
    let home = Home::new("flow");

    let made = home.run(&[
        "job",
        "new",
        "demo",
        "--template",
        "shell",
        "--feed",
        "--handoff",
    ]);
    assert!(made.status.success(), "{}", text(&made.stderr));
    let out = text(&made.stdout);
    assert!(out.contains("created"), "{out}");
    assert!(out.contains("flockr job validate demo --run-feed"), "{out}");
    assert!(out.contains("flockr job enable demo"), "{out}");
    let dir = home.jobs().join("demo");
    for file in ["job.toml", "run.sh", "feed.sh", "prompt.md"] {
        assert!(dir.join(file).is_file(), "{file}");
    }
    assert!(std::fs::read_to_string(dir.join("job.toml"))
        .unwrap()
        .contains("enabled = false"));

    // A second `new` with the same id is refused and changes nothing.
    std::fs::write(dir.join("run.sh"), "echo mine\n").unwrap();
    let again = home.run(&["job", "new", "demo", "--template", "python"]);
    assert_eq!(again.status.code(), Some(1));
    assert!(
        text(&again.stderr).contains("already exists"),
        "{}",
        text(&again.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("run.sh")).unwrap(),
        "echo mine\n"
    );
    assert!(!dir.join("run.py").exists());
    std::fs::write(dir.join("run.sh"), "echo hello from demo\n").unwrap();

    let validated = home.run(&["job", "validate", "demo", "--run-feed"]);
    let out = text(&validated.stdout);
    assert!(
        validated.status.success(),
        "{out}\n{}",
        text(&validated.stderr)
    );
    assert!(out.contains("result: ok"), "{out}");
    assert!(out.contains("ran: 1 items, 1 unread"), "{out}");

    let json = home.run(&["job", "validate", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["jobs"][0]["id"], "demo");

    // The state directory must not even be created by a dry run.
    let before = listing(&home.root);
    let dry = home.run(&["job", "run", "demo", "--dry-run"]);
    let out = text(&dry.stdout);
    assert!(dry.status.success(), "{out}\n{}", text(&dry.stderr));
    assert!(out.contains("would ask"), "{out}");
    assert!(out.contains("1 items need you. Start Claude?"), "{out}");
    assert!(out.contains("hello from demo"), "{out}");
    assert_eq!(
        listing(&home.root),
        before,
        "a dry run must not write anything"
    );
    assert!(!home.state().exists());

    let no_exec = home.run(&["job", "run", "demo", "--dry-run", "--no-exec"]);
    assert!(no_exec.status.success());
    assert!(text(&no_exec.stdout).contains("--no-exec"));
    assert_eq!(listing(&home.root), before);
}

#[test]
fn validate_exits_non_zero_on_errors() {
    let home = Home::new("errors");
    assert!(home
        .run(&["job", "new", "bad", "--template", "shell"])
        .status
        .success());
    let dir = home.jobs().join("bad");
    std::fs::write(
        dir.join("job.toml"),
        "command = [\"sh\", \"gone.sh\"]\nevery = \"1h\"\n",
    )
    .unwrap();
    let out = home.run(&["job", "validate", "bad"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stdout).contains("error: command: the script gone.sh does not exist"));
}

#[test]
fn templates_schemas_and_the_skill_print() {
    let home = Home::new("print");
    for template in ["python", "rust", "workbox"] {
        let id = format!("t-{template}");
        let made = home.run(&["job", "new", &id, "--template", template, "--every", "30m"]);
        assert!(made.status.success(), "{template}: {}", text(&made.stderr));
        assert!(home.jobs().join(&id).join("job.toml").is_file());
    }
    let rust = home.run(&["job", "new", "r2", "--template", "rust"]);
    let out = text(&rust.stdout);
    assert!(out.contains("cargo install rust-script"), "{out}");
    assert!(out.contains("flockr job build r2"), "{out}");

    let bad = home.run(&["job", "new", "x", "--template", "go"]);
    assert_eq!(bad.status.code(), Some(2));
    let bad = home.run(&["job", "new", "x", "--every", "0 9 * * *"]);
    assert_eq!(bad.status.code(), Some(2));
    let bad = home.run(&["job", "new", "../escape"]);
    assert_eq!(bad.status.code(), Some(1));

    let schema = home.run(&["job", "schema"]);
    let value: serde_json::Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert_eq!(value["title"], "flockr job.toml");
    let feed = home.run(&["job", "schema", "--feed"]);
    let value: serde_json::Value = serde_json::from_slice(&feed.stdout).unwrap();
    assert_eq!(value["title"], "flockr feed v1");

    let skill = home.run(&["job", "skill"]);
    assert!(text(&skill.stdout).starts_with("---\nname: flockr-jobs\n"));
    let built = home.run(&["job", "build", "t-python"]);
    assert!(
        text(&built.stdout).contains("nothing to build"),
        "{}",
        text(&built.stdout)
    );
}
