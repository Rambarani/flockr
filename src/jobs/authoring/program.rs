//! Finding a program the way a job would start it: by `PATH`, or relative to
//! the job's working directory when the name has a separator.

use std::path::{Path, PathBuf};

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn candidates(base: PathBuf) -> Vec<PathBuf> {
    vec![base]
}

#[cfg(not(unix))]
fn candidates(base: PathBuf) -> Vec<PathBuf> {
    let mut all = vec![base.clone()];
    if base.extension().is_none() {
        for extension in ["exe", "cmd", "bat", "com"] {
            all.push(base.with_extension(extension));
        }
    }
    all
}

/// The file `program` would start. A name with a path separator is relative
/// to `cwd` (or absolute); a bare name is searched for in `path_var`.
pub(crate) fn find_program(program: &str, cwd: &Path, path_var: Option<&str>) -> Option<PathBuf> {
    let has_separator = program.contains('/') || (cfg!(windows) && program.contains('\\'));
    if has_separator {
        let path = crate::jobs::definition::expand_tilde(program);
        let path = if path.is_absolute() {
            path
        } else {
            crate::plugin_command::program_for_cwd(program, cwd)
        };
        return candidates(path)
            .into_iter()
            .find(|candidate| is_executable(candidate));
    }
    std::env::split_paths(path_var?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidates(dir.join(program)))
        .find(|candidate| is_executable(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn finds_programs_on_the_path_and_relative_to_the_cwd() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("flockr-program-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.join("plain");
        std::fs::write(&plain, "x").unwrap();

        let path = dir.display().to_string();
        assert_eq!(
            find_program("tool", Path::new("/"), Some(&path)),
            Some(tool.clone())
        );
        assert_eq!(find_program("plain", Path::new("/"), Some(&path)), None);
        assert_eq!(find_program("missing", Path::new("/"), Some(&path)), None);
        assert_eq!(find_program("tool", Path::new("/"), None), None);
        assert!(find_program("./tool", &dir, None).is_some());
        assert!(find_program("./plain", &dir, None).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
