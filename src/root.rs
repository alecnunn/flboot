use std::path::{Path, PathBuf};

pub fn find_repo_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    find_repo_root_from(&start)
}

fn find_repo_root_from(start: &Path) -> anyhow::Result<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            anyhow::bail!(
                "could not find repo root (no .git directory found walking up from {})",
                start.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_git_dir_in_ancestor() {
        let base = std::env::temp_dir().join(format!("flboot-root-test-{}", std::process::id()));
        let nested = base.join("tools").join("flboot");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(base.join(".git")).unwrap();

        let found = find_repo_root_from(&nested).unwrap();

        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(found, base);
    }
}
