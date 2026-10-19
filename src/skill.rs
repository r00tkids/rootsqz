//! `rootsqz install-skill`: installs the agent skill about the compression report into a project.

use std::{fs, path::PathBuf};

use anyhow::{bail, Context, Result};

/// The skill, as in the repository, so that it matches the version of this executable.
pub const SKILL: &str = include_str!("../skills/rootsqz-report/SKILL.md");

/// Where agents look for skills in a project: Claude Code, and agents of the open skills format.
const SKILL_DIRS: [&str; 2] = [".claude/skills", ".agents/skills"];

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Project directory to install the skill into
    #[arg(default_value = ".")]
    pub dir: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    if !args.dir.is_dir() {
        bail!("'{}' is not a directory", args.dir.display());
    }
    for skills in SKILL_DIRS {
        let path = args
            .dir
            .join(skills)
            .join("rootsqz-report")
            .join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap())
            .with_context(|| format!("Failed to create the directory of {}", path.display()))?;
        fs::write(&path, SKILL).with_context(|| format!("Failed to write {}", path.display()))?;
        println!("Wrote {}", path.display());
    }
    println!(
        "Agents now find out from it how to make and read a compression report when asked to make the intro smaller. Run this again after updating rootsqz (v{}).",
        env!("CARGO_PKG_VERSION")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn skill_has_frontmatter() {
        let front = super::SKILL.strip_prefix("---\n").unwrap();
        let (front, _) = front.split_once("\n---\n").unwrap();
        assert!(front.contains("name: rootsqz-report"));
        assert!(front.contains("description: "));
    }

    #[test]
    fn install() {
        let dir = std::env::temp_dir().join(format!("rootsqz-skill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        super::run(super::Args { dir: dir.clone() }).unwrap();
        for skills in super::SKILL_DIRS {
            let written =
                std::fs::read_to_string(dir.join(skills).join("rootsqz-report/SKILL.md")).unwrap();
            assert_eq!(written, super::SKILL);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
