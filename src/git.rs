use std::fs;
use std::path::{Path, PathBuf};

/// Current branch of the repository or worktree containing `cwd`, read from
/// `HEAD` without spawning git. Returns a short SHA for a detached HEAD.
pub fn branch(cwd: &Path) -> Option<String> {
    let git_dir = find_git_dir(cwd)?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(r) => Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string()),
        None => Some(head.chars().take(8).collect()),
    }
}

fn find_git_dir(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            // Worktrees and submodules: `.git` is a file with `gitdir: <path>`.
            let text = fs::read_to_string(&dot_git).ok()?;
            let target = text.trim().strip_prefix("gitdir: ")?;
            let target = Path::new(target);
            return Some(if target.is_absolute() {
                target.to_path_buf()
            } else {
                dir.join(target)
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_branch_from_worktree_gitdir_file() {
        let tmp = tempfile::tempdir().unwrap();
        let gitdir = tmp.path().join("main/.git/worktrees/wt");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/feat/x\n").unwrap();
        let wt = tmp.path().join("wt/sub");
        fs::create_dir_all(&wt).unwrap();
        fs::write(
            tmp.path().join("wt/.git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        assert_eq!(branch(&wt).as_deref(), Some("feat/x"));
    }

    #[test]
    fn detached_head_is_short_sha() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".git")).unwrap();
        fs::write(tmp.path().join(".git/HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(branch(tmp.path()).as_deref(), Some("01234567"));
    }
}
