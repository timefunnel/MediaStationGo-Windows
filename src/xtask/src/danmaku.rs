use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

pub fn stage(out: &Path) -> Result<()> {
    let root = crate::paths::repo_root().join("resources/danmaku");
    for args in [
        vec!["ci", "--ignore-scripts", "--no-audit", "--no-fund"],
        vec!["run", "build"],
    ] {
        let mut command = if cfg!(windows) {
            let mut cmd = Command::new("cmd.exe");
            cmd.args(["/d", "/c", "npm.cmd"]);
            cmd
        } else {
            Command::new("npm")
        };
        let status = command
            .args(args)
            .current_dir(&root)
            .status()
            .context("build bundled local danmaku module")?;
        if !status.success() {
            bail!("local danmaku module build failed");
        }
    }
    crate::fs::copy_dir_recursive(&root.join("dist"), &out.join("danmaku"))?;
    Ok(())
}
