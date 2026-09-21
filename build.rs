use std::process::Command;

fn main() {
    // ponytail: shells out to git; "unknown" when built outside a checkout.
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .is_ok_and(|o| !o.stdout.is_empty());
    println!("cargo:rustc-env=GIT_HASH={hash}{}", if dirty { "*" } else { "" });
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
