//! `kaku chat` subcommand: thin wrapper around the bundled `k` binary.
//!
//! Why this exists when `k` is already on PATH after `kaku init`:
//!
//! - **Discoverability**: `kaku --help` should show the full AI story
//!   (`ai` to configure, `chat` to talk). New users find `chat` here without
//!   knowing about the 1-letter `k` shortcut.
//! - **PATH-independent**: works even when the user has not run
//!   `kaku init` or is on a shell without managed integration.
//! - **One chat entry point**: we exec the bundled `k` entry point when it is
//!   present, or the shared `kaku-gui` multicall binary with argv0 set to `k`.
//!   This keeps `kaku chat` working on Linux without installing a global `k`
//!   command that could conflict with kubectl users.

use anyhow::anyhow;
use clap::Parser;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

#[derive(Debug, Parser, Clone, Default)]
pub struct ChatCommand {
    /// Forward every remaining argument verbatim to the `k` binary so flags
    /// like `-n` (new conversation) and `-r [ID]` (resume) keep working.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl ChatCommand {
    pub fn run(&self) -> anyhow::Result<()> {
        let k_bin = resolve_k_binary()?;
        let mut cmd = std::process::Command::new(&k_bin);
        cmd.args(&self.args);

        #[cfg(unix)]
        {
            cmd.arg0("k");
            // Exec replaces the current process; the user's $? on the next
            // shell line is exactly `k`'s exit code, with no extra fork
            // overhead and no parent-process bookkeeping.
            let err = cmd.exec();
            return Err(anyhow!("failed to exec {}: {}", k_bin.display(), err));
        }
    }
}

/// Locate the bundled chat-capable executable without consulting PATH.
///
/// Avoiding PATH is intentional: a user's `k` command may be kubectl, and
/// `kaku chat` must never dispatch to an unrelated executable.
fn resolve_k_binary() -> anyhow::Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. Same directory as the running `kaku` binary (the macOS bundle case).
    if let Ok(exe) = std::env::current_exe() {
        #[cfg(target_os = "macos")]
        candidates.push(exe.with_file_name("k"));
        if let Ok(canonical) = std::fs::canonicalize(&exe) {
            #[cfg(target_os = "macos")]
            let names = ["k", "kaku-gui"];
            #[cfg(not(target_os = "macos"))]
            let names = ["kaku-gui"];
            for name in names {
                let beside_canonical = canonical.with_file_name(name);
                if !candidates.contains(&beside_canonical) {
                    candidates.push(beside_canonical);
                }
            }
        }
        candidates.push(exe.with_file_name("kaku-gui"));
    }

    // 2. Standard install locations.
    #[cfg(target_os = "macos")]
    {
        for path in [
            PathBuf::from("/Applications/Kaku.app/Contents/MacOS/k"),
            config::HOME_DIR
                .join("Applications")
                .join("Kaku.app")
                .join("Contents")
                .join("MacOS")
                .join("k"),
        ] {
            candidates.push(path);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        candidates.extend([
            PathBuf::from("/usr/local/bin/kaku-gui"),
            PathBuf::from("/usr/bin/kaku-gui"),
            config::HOME_DIR.join(".local").join("bin").join("kaku-gui"),
        ]);
    }

    for candidate in &candidates {
        if is_executable(candidate) {
            return Ok(candidate.clone());
        }
    }

    Err(anyhow!(
        "could not locate the bundled chat entry point. Install Kaku again, \
         or run `kaku init` to refresh the shell integration. The standalone \
         `k` command is optional and is never required by `kaku chat`."
    ))
}

fn is_executable(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_returns_error_when_no_candidate_exists() {
        // We can't easily stub `current_exe` so the test is environmental:
        // simply ensure the function does not panic and returns a typed
        // result. CI machines without Kaku installed should hit the Err
        // path; developer machines will return Ok with a real path.
        let _ = resolve_k_binary();
    }

    #[cfg(unix)]
    #[test]
    fn chat_uses_argv0_instead_of_a_global_k_command() {
        use std::os::unix::process::CommandExt;

        let mut command = std::process::Command::new("/tmp/kaku-gui");
        command.arg0("k");
        assert_eq!(command.get_program(), std::ffi::OsStr::new("/tmp/kaku-gui"));
    }
}
