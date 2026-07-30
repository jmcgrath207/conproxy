use std::process::Command;

/// Stop a Docker container by name.
pub fn stop_container(name: &str) {
    let _ = Command::new("docker").args(["stop", name]).output();
}

/// Start a Docker container by name.
pub fn start_container(name: &str) {
    let _ = Command::new("docker").args(["start", name]).output();
}
