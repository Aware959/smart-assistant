use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn main() {
    let build_number = git(&["rev-list", "--count", "HEAD"]).unwrap_or_else(|| "0".to_string());
    let build_commit =
        git(&["rev-parse", "--short=10", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain"])
        .map(|status| !status.trim().is_empty())
        .unwrap_or(false);

    println!("cargo:rustc-env=SMART_ASSISTANT_BUILD_NUMBER={build_number}");
    println!("cargo:rustc-env=SMART_ASSISTANT_BUILD_COMMIT={build_commit}");
    println!(
        "cargo:rustc-env=SMART_ASSISTANT_BUILD_DIRTY={}",
        u8::from(dirty)
    );
}
