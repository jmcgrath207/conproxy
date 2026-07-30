use super::constants::{EvalConfig, Vertical};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set the child process to be a process group leader so we can kill the
/// entire tree (claude + any subprocesses it spawns) on timeout.
#[cfg(unix)]
fn set_process_group_leader(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: pre_exec runs between fork and exec. setpgid(0,0) makes
    // the child its own process group leader. No allocations or locks.
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn set_process_group_leader(_cmd: &mut Command) {}

/// Result of a single Claude CLI invocation.
#[allow(dead_code)]
pub struct ClaudeInvocation {
    pub query: String,
    pub vertical: Vertical,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration: Duration,
    pub success: bool,
    pub timed_out: bool,
}

/// Wraps Claude CLI subprocess invocations.
pub struct ClaudeRunner {
    claude_bin: PathBuf,
    timeout: Duration,
    model: Option<String>,
}

impl ClaudeRunner {
    pub fn new(config: &EvalConfig) -> Self {
        Self {
            claude_bin: config.claude_bin.clone(),
            timeout: config.timeout,
            model: config.claude_model.clone(),
        }
    }

    /// Invoke Claude CLI for a given vertical and query.
    pub fn invoke(
        &self,
        vertical: Vertical,
        query: &str,
        mcp_config_path: Option<&Path>,
        working_dir: &Path,
    ) -> ClaudeInvocation {
        let prompt = self.build_prompt(vertical, query);

        let mut cmd = self.build_host_cmd(vertical, &prompt, mcp_config_path, working_dir);

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        set_process_group_leader(&mut cmd);

        let start = Instant::now();
        let timeout = self.timeout;
        self.invoke_with_timeout(&mut cmd, query, vertical, start, timeout)
    }

    /// Build a direct host command to run Claude as a subprocess.
    fn build_host_cmd(
        &self,
        vertical: Vertical,
        prompt: &str,
        mcp_config_path: Option<&Path>,
        working_dir: &Path,
    ) -> Command {
        let mut cmd = Command::new(&self.claude_bin);

        cmd.args([
            "-p",
            "--dangerously-skip-permissions",
            "--no-session-persistence",
        ]);
        if let Some(ref model) = self.model {
            cmd.args(["--model", model]);
        }
        cmd.args(["--setting-sources", ""]);

        self.append_vertical_args(&mut cmd, vertical, mcp_config_path);
        // `--` separates options from positional args — needed because variadic
        // flags like --allowedTools, --tools, --mcp-config consume all following
        // args and would swallow the prompt.
        cmd.arg("--");
        cmd.arg(prompt);
        cmd.current_dir(working_dir);

        // Remove CLAUDECODE env var so the subprocess doesn't think it's a
        // nested session and refuse to start.
        cmd.env_remove("CLAUDECODE");

        cmd
    }

    /// Append vertical-specific CLI flags.
    fn append_vertical_args(
        &self,
        cmd: &mut Command,
        vertical: Vertical,
        mcp_config_path: Option<&Path>,
    ) {
        match vertical {
            Vertical::NoContext => {
                // Disable all tools so Claude can't read the filesystem.
                cmd.args(["--tools", ""]);
            }
            Vertical::McpTools => {
                if let Some(mcp_path) = mcp_config_path {
                    cmd.args(["--mcp-config", &mcp_path.to_string_lossy()]);
                }
            }
        }
    }

    fn build_prompt(&self, vertical: Vertical, query: &str) -> String {
        match vertical {
            Vertical::NoContext => {
                format!(
                    "Answer the following question with specific technical details, names, and concepts.\n\n\
                     Question: {query}"
                )
            }
            Vertical::McpTools => {
                format!(
                    "You have access to conproxy MCP tools: search.\n\
                     Use search to find relevant documents and answer with specific technical details.\n\n\
                     Question: {query}"
                )
            }
        }
    }

    fn invoke_with_timeout(
        &self,
        cmd: &mut Command,
        query: &str,
        vertical: Vertical,
        start: Instant,
        timeout: Duration,
    ) -> ClaudeInvocation {
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return ClaudeInvocation {
                    query: query.to_string(),
                    vertical,
                    stdout: String::new(),
                    stderr: format!("Failed to spawn claude: {e}"),
                    exit_code: None,
                    duration: start.elapsed(),
                    success: false,
                    timed_out: false,
                };
            }
        };

        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();

        // Watchdog thread: kills the entire process group after timeout
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            if rx.try_recv().is_err() {
                #[cfg(unix)]
                unsafe {
                    // Negative PID = kill the entire process group
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
        });

        let output = child.wait_with_output();
        let duration = start.elapsed();
        let _ = tx.send(()); // cancel watchdog

        let timed_out = duration >= timeout;

        match output {
            Ok(output) => ClaudeInvocation {
                query: query.to_string(),
                vertical,
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code(),
                duration,
                success: output.status.success(),
                timed_out,
            },
            Err(e) => ClaudeInvocation {
                query: query.to_string(),
                vertical,
                stdout: String::new(),
                stderr: format!("Failed to wait for claude: {e}"),
                exit_code: None,
                duration,
                success: false,
                timed_out,
            },
        }
    }
}
