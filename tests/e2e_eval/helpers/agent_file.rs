use super::constants::{EvalProvider, Vertical};
use std::path::{Path, PathBuf};

/// Prepare an isolated working directory for a vertical under the eval base dir.
///
/// For McpTools: writes a `.conproxy/conproxy.toml` with the eval proxy as the
/// sole upstream so the running conproxy can route MCP-driven queries to it.
/// NoContext gets no conproxy setup.
///
/// Only runs `git init` when `provider == Claude` (Claude CLI needs it to detect project root).
pub fn prepare_vertical_dir(
    vertical: Vertical,
    base_dir: &Path,
    _conproxy_bin: &Path,
    proxy_listen: &str,
    provider: EvalProvider,
) -> PathBuf {
    let dir = base_dir.join("verticals").join(vertical.name());
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("Failed to create vertical dir {}: {e}", dir.display()));

    // Git init only needed for Claude CLI to detect project root and load CLAUDE.md.
    if provider == EvalProvider::Claude {
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .output();
    }

    // NoContext gets no conproxy setup
    if vertical == Vertical::NoContext {
        return dir;
    }

    // Write `.conproxy/conproxy.toml` directly. `conproxy start` no longer
    // requires a prior init step, so we can lay out the project without
    // shelling out to the binary.
    let conproxy_dir = dir.join(".conproxy");
    std::fs::create_dir_all(&conproxy_dir)
        .unwrap_or_else(|e| panic!("Failed to create .conproxy in {}: {e}", dir.display()));
    let toml = format!(
        "[server]\nlisten = \"{proxy_listen}\"\n\n\
         [upstreams.eval]\n\
         url = \"http://{proxy_listen}\"\n\
         type = \"qdrant\"\n\
         timeout_secs = 30\n\n\
         [contexts.default]\ndefault = true\n\n\
         [[contexts.default.upstreams]]\n\
         ref = \"eval\"\npriority = 0\n"
    );
    std::fs::write(conproxy_dir.join("conproxy.toml"), toml)
        .unwrap_or_else(|e| panic!("Failed to write conproxy.toml in {}: {e}", dir.display()));

    dir
}
