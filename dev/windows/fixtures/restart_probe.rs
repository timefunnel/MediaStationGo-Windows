//! GUI-subsystem fixture for test_app_restart.ps1; never shipped with the app.
#![windows_subsystem = "windows"]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = args.first().ok_or("probe result path is missing")?;
    let cwd = std::env::current_dir()?;
    let evidence = std::iter::once(cwd.to_str().ok_or("probe directory is not Unicode")?)
        .chain(args.iter().skip(1).map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(result, evidence)?;
    Ok(())
}
