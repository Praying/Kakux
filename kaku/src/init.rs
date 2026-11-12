use anyhow::{anyhow, bail, Context};
use clap::Parser;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Parser, Clone, Default)]
pub struct InitCommand {
    /// Refresh shell integration without interactive prompts
    #[arg(long)]
    pub update_only: bool,

    /// Shell integration to configure
    #[arg(long, value_enum)]
    pub shell: Option<crate::shell::ManagedShell>,
}

impl InitCommand {
    pub fn run(&self) -> anyhow::Result<()> {
        imp::run(self.update_only, self.shell)
    }
}

/// Binary and resource lookup differs per platform: macOS resolves through
/// the Kaku.app bundle, Linux through the system install prefix.
mod imp {
    use super::*;
    use crate::shell::{
        find_shell_executable, persist_initialized_state, persist_managed_shell,
        preferred_managed_shell, ManagedShell,
    };
    use std::os::unix::fs::PermissionsExt;

    /// Absolute candidate locations for the `kaku`/`k` executables, most
    /// preferred first.
    fn bin_candidates(bin_name: &str) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            candidates.push(exe.with_file_name(bin_name));
        }
        #[cfg(target_os = "macos")]
        candidates.extend([
            PathBuf::from("/Applications/Kaku.app/Contents/MacOS").join(bin_name),
            config::HOME_DIR
                .join("Applications")
                .join("Kaku.app")
                .join("Contents")
                .join("MacOS")
                .join(bin_name),
        ]);
        #[cfg(not(target_os = "macos"))]
        candidates.extend([
            PathBuf::from("/usr/local/bin").join(bin_name),
            PathBuf::from("/usr/bin").join(bin_name),
            config::HOME_DIR.join(".local").join("bin").join(bin_name),
        ]);
        candidates
    }

    /// Shell snippet listing the candidate binaries for the wrapper scripts.
    fn wrapper_candidate_lines(bin_name: &str) -> String {
        let candidates = bin_candidates(bin_name);
        candidates
            .iter()
            .map(|p| {
                format!(
                    "\t\"{}\" \\",
                    escape_for_double_quotes(&p.display().to_string())
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn run(update_only: bool, shell: Option<ManagedShell>) -> anyhow::Result<()> {
        let shell = select_shell(update_only, shell)?;
        if find_shell_executable(shell).is_none() {
            bail!(
                "cannot configure {name}: no `{name}` executable found on PATH or in \
                 standard locations. Install it, or pick the other shell with \
                 `kaku init --shell <shell>`.",
                name = shell.name()
            );
        }
        // Record the selection before running setup so a partially failed run
        // retries against the same shell instead of re-detecting from $SHELL.
        persist_managed_shell(shell).context("remember selected shell")?;
        ensure_user_config().context("ensure user config exists")?;

        install_kaku_wrapper(shell).context("install kaku wrapper")?;
        install_k_wrapper(shell).context("install k wrapper")?;

        let script_name = match shell {
            ManagedShell::Fish => "setup_fish.sh",
            ManagedShell::Zsh => "setup_zsh.sh",
        };
        let script = resolve_setup_script(script_name)
            .ok_or_else(|| anyhow!("failed to locate {} for Kaku initialization", script_name))?;

        let mut cmd = Command::new("/bin/bash");
        cmd.arg(&script)
            .env("KAKU_INIT_INTERNAL", "1")
            .env("KAKU_TARGET_SHELL", shell.name());
        if update_only {
            cmd.arg("--update-only");
        }
        let status = cmd
            .status()
            .with_context(|| format!("run {}", script.display()))?;

        if status.success() {
            let config_version = read_setup_config_version(&script)?;
            persist_initialized_state(shell, config_version)
                .context("record completed shell initialization")?;
            return Ok(());
        }

        bail!("kaku init failed with status {}", status);
    }

    fn select_shell(
        update_only: bool,
        selected: Option<ManagedShell>,
    ) -> anyhow::Result<ManagedShell> {
        if let Some(shell) = selected {
            return Ok(shell);
        }

        let default = preferred_managed_shell();
        if update_only || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Ok(default);
        }

        let Some(zsh_path) = find_shell_executable(ManagedShell::Zsh) else {
            return Ok(default);
        };
        let Some(fish_path) = find_shell_executable(ManagedShell::Fish) else {
            return Ok(default);
        };

        prompt_for_shell(default, &zsh_path, &fish_path)
    }

    fn prompt_for_shell(
        default: ManagedShell,
        zsh_path: &Path,
        fish_path: &Path,
    ) -> anyhow::Result<ManagedShell> {
        println!("Which shell should Kaku configure?");
        println!(
            "  1) zsh  ({}){}",
            zsh_path.display(),
            shell_default_label(default, ManagedShell::Zsh)
        );
        println!(
            "  2) fish ({}){}",
            fish_path.display(),
            shell_default_label(default, ManagedShell::Fish)
        );

        loop {
            print!("Select [{}]: ", shell_number(default));
            io::stdout().flush().context("flush shell prompt")?;

            let mut input = String::new();
            let bytes = io::stdin()
                .read_line(&mut input)
                .context("read shell selection")?;
            if bytes == 0 {
                return Ok(default);
            }
            if let Some(shell) = parse_shell_selection(&input, default) {
                return Ok(shell);
            }
            eprintln!("Choose 1 for zsh or 2 for fish.");
        }
    }

    fn shell_default_label(default: ManagedShell, shell: ManagedShell) -> &'static str {
        if default == shell {
            " (detected login shell)"
        } else {
            ""
        }
    }

    fn shell_number(shell: ManagedShell) -> u8 {
        match shell {
            ManagedShell::Zsh => 1,
            ManagedShell::Fish => 2,
        }
    }

    fn parse_shell_selection(input: &str, default: ManagedShell) -> Option<ManagedShell> {
        match input.trim().to_ascii_lowercase().as_str() {
            "" => Some(default),
            "1" | "zsh" => Some(ManagedShell::Zsh),
            "2" | "fish" => Some(ManagedShell::Fish),
            _ => None,
        }
    }

    fn install_kaku_wrapper(shell: ManagedShell) -> anyhow::Result<()> {
        let wrapper_path = wrapper_path(shell);
        let wrapper_dir = wrapper_path
            .parent()
            .ok_or_else(|| anyhow!("invalid wrapper path"))?;
        config::create_user_owned_dirs(wrapper_dir).context("create wrapper directory")?;

        if fs::symlink_metadata(&wrapper_path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            fs::remove_file(&wrapper_path).with_context(|| {
                format!("remove legacy symlink wrapper {}", wrapper_path.display())
            })?;
        }

        let preferred_bin = resolve_preferred_kaku_bin();
        let preferred_line = preferred_bin
            .map(|p| {
                format!(
                    "\t\"{}\" \\\n",
                    escape_for_double_quotes(&p.display().to_string())
                )
            })
            .unwrap_or_default();
        let candidate_lines = wrapper_candidate_lines("kaku");

        let script = format!(
            r#"#!/bin/bash
set -euo pipefail

if [[ -n "${{KAKU_BIN:-}}" && -x "${{KAKU_BIN}}" ]]; then
	exec "${{KAKU_BIN}}" "$@"
fi

for candidate in \
{preferred_line}{candidate_lines}; do
	if [[ -n "$candidate" && -x "$candidate" ]]; then
		exec "$candidate" "$@"
	fi
done

echo "kaku: no kaku executable found. Run 'kaku init' after installing Kaku." >&2
exit 127
"#
        );

        let mut file = fs::File::create(&wrapper_path)
            .with_context(|| format!("create wrapper {}", wrapper_path.display()))?;
        file.write_all(script.as_bytes())
            .with_context(|| format!("write wrapper {}", wrapper_path.display()))?;
        fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod wrapper {}", wrapper_path.display()))?;
        Ok(())
    }

    fn install_k_wrapper(shell: ManagedShell) -> anyhow::Result<()> {
        let k_path = k_wrapper_path(shell);
        let k_dir = k_path
            .parent()
            .ok_or_else(|| anyhow!("invalid k wrapper path"))?;
        config::create_user_owned_dirs(k_dir).context("create k wrapper directory")?;

        // If something else already owns this path and it is not our wrapper, skip.
        if k_path.exists() {
            let content = fs::read_to_string(&k_path).unwrap_or_default();
            if !content.contains("Kaku") && !content.contains("kaku") {
                eprintln!(
                    "k: {} already exists and does not appear to be a Kaku wrapper; skipping.",
                    k_path.display()
                );
                return Ok(());
            }
        }
        if fs::symlink_metadata(&k_path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            fs::remove_file(&k_path)
                .with_context(|| format!("remove legacy symlink k wrapper {}", k_path.display()))?;
        }

        let preferred_line = resolve_preferred_k_bin()
            .map(|p| {
                format!(
                    "\t\"{}\" \\\n",
                    escape_for_double_quotes(&p.display().to_string())
                )
            })
            .unwrap_or_default();
        let candidate_lines = wrapper_candidate_lines("k");

        let script = format!(
            r#"#!/bin/bash
set -euo pipefail

for candidate in \
{preferred_line}{candidate_lines}; do
	if [[ -n "$candidate" && -x "$candidate" ]]; then
		exec "$candidate" "$@"
	fi
done

echo "k: no k executable found. Run 'kaku init' after installing Kaku." >&2
exit 127
"#
        );

        let mut file = fs::File::create(&k_path)
            .with_context(|| format!("create k wrapper {}", k_path.display()))?;
        file.write_all(script.as_bytes())
            .with_context(|| format!("write k wrapper {}", k_path.display()))?;
        fs::set_permissions(&k_path, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod k wrapper {}", k_path.display()))?;
        Ok(())
    }

    fn k_wrapper_path(shell: ManagedShell) -> PathBuf {
        let dir = match shell {
            ManagedShell::Fish => "fish",
            ManagedShell::Zsh => "zsh",
        };
        config::HOME_DIR
            .join(".config")
            .join("kaku")
            .join(dir)
            .join("bin")
            .join("k")
    }

    fn resolve_preferred_k_bin() -> Option<PathBuf> {
        bin_candidates("k")
            .into_iter()
            .find(|p| is_executable_file(p))
    }

    fn wrapper_path(shell: ManagedShell) -> PathBuf {
        let dir = match shell {
            ManagedShell::Fish => "fish",
            ManagedShell::Zsh => "zsh",
        };
        config::HOME_DIR
            .join(".config")
            .join("kaku")
            .join(dir)
            .join("bin")
            .join("kaku")
    }

    fn resolve_preferred_kaku_bin() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("KAKU_BIN") {
            let path = PathBuf::from(path);
            if is_executable_file(&path) {
                return Some(path);
            }
        }

        if let Ok(exe) = std::env::current_exe() {
            if exe
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.eq_ignore_ascii_case("kaku"))
                .unwrap_or(false)
                && is_executable_file(&exe)
            {
                return Some(exe);
            }
        }

        bin_candidates("kaku")
            .into_iter()
            .find(|p| is_executable_file(p))
    }

    fn is_executable_file(path: &Path) -> bool {
        fs::metadata(path)
            .map(|meta| meta.is_file() && (meta.permissions().mode() & 0o111 != 0))
            .unwrap_or(false)
    }

    fn escape_for_double_quotes(value: &str) -> String {
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('`', "\\`")
    }

    fn resolve_setup_script(script_name: &str) -> Option<PathBuf> {
        let cwd = std::env::current_dir().ok();
        let exe = std::env::current_exe().ok();
        // Only debug builds trust the working directory, so a release build
        // run inside any checkout still uses the bundled script.
        setup_script_candidates(
            script_name,
            cwd.as_deref(),
            exe.as_deref(),
            cfg!(debug_assertions),
        )
        .into_iter()
        .find(|p| p.exists())
    }

    fn setup_script_candidates(
        script_name: &str,
        cwd: Option<&Path>,
        exe: Option<&Path>,
        include_cwd: bool,
    ) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        if let Some(cwd) = cwd.filter(|_| include_cwd) {
            candidates.push(
                cwd.join("assets")
                    .join("shell-integration")
                    .join(script_name),
            );
        }

        if let Some(exe) = exe {
            #[cfg(target_os = "macos")]
            if let Some(contents_dir) = exe.parent().and_then(|p| p.parent()) {
                candidates.push(contents_dir.join("Resources").join(script_name));
            }
            // Linux layout: an exe in <prefix>/bin resolves its resources
            // from <prefix>/share/kaku.
            #[cfg(not(target_os = "macos"))]
            if let Some(prefix_dir) = exe.parent().and_then(|p| p.parent()) {
                candidates.push(prefix_dir.join("share").join("kaku").join(script_name));
            }
        }

        #[cfg(target_os = "macos")]
        {
            candidates.push(PathBuf::from(format!(
                "/Applications/Kaku.app/Contents/Resources/{}",
                script_name
            )));
            candidates.push(
                config::HOME_DIR
                    .join("Applications")
                    .join("Kaku.app")
                    .join("Contents")
                    .join("Resources")
                    .join(script_name),
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            candidates.push(PathBuf::from("/usr/local/share/kaku").join(script_name));
            candidates.push(PathBuf::from("/usr/share/kaku").join(script_name));
        }

        candidates
    }

    fn read_setup_config_version(setup_script: &Path) -> anyhow::Result<u64> {
        let version_file = setup_script
            .parent()
            .context("setup script has no resource directory")?
            .join("config_version.txt");
        let raw = fs::read_to_string(&version_file)
            .with_context(|| format!("read {}", version_file.display()))?;
        raw.trim()
            .parse::<u64>()
            .with_context(|| format!("parse bundled config version in {}", version_file.display()))
    }

    fn ensure_user_config() -> anyhow::Result<()> {
        config::ensure_user_config_exists().context("ensure user config exists")?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn shell_prompt_accepts_numbers_names_and_default() {
            assert_eq!(
                parse_shell_selection("", ManagedShell::Fish),
                Some(ManagedShell::Fish)
            );
            assert_eq!(
                parse_shell_selection("1", ManagedShell::Fish),
                Some(ManagedShell::Zsh)
            );
            assert_eq!(
                parse_shell_selection("fish", ManagedShell::Zsh),
                Some(ManagedShell::Fish)
            );
            assert_eq!(parse_shell_selection("bash", ManagedShell::Zsh), None);
        }

        #[test]
        fn setup_version_comes_from_the_setup_resource_directory() {
            let root = tempfile::tempdir().unwrap();
            let script = root.path().join("setup_zsh.sh");
            fs::write(&script, "#!/bin/bash\n").unwrap();
            fs::write(root.path().join("config_version.txt"), "31\n").unwrap();

            assert_eq!(read_setup_config_version(&script).unwrap(), 31);
        }

        fn checkout() -> PathBuf {
            PathBuf::from("/Users/someone/src/Kaku")
        }

        fn app_exe() -> PathBuf {
            #[cfg(target_os = "macos")]
            {
                PathBuf::from("/Applications/Kaku.app/Contents/MacOS/kaku")
            }
            #[cfg(not(target_os = "macos"))]
            {
                PathBuf::from("/usr/bin/kaku")
            }
        }

        #[test]
        fn release_setup_script_candidates_skip_the_working_directory() {
            let cwd = checkout();
            let exe = app_exe();
            let candidates = setup_script_candidates("setup_zsh.sh", Some(&cwd), Some(&exe), false);

            assert!(
                candidates.iter().all(|p| !p.starts_with(&cwd)),
                "release candidates must not come from the working directory: {:?}",
                candidates
            );
            #[cfg(target_os = "macos")]
            let expected_first =
                PathBuf::from("/Applications/Kaku.app/Contents/Resources/setup_zsh.sh");
            #[cfg(not(target_os = "macos"))]
            let expected_first = PathBuf::from("/usr/share/kaku/setup_zsh.sh");
            assert_eq!(
                candidates.first(),
                Some(&expected_first),
                "the bundled script comes first: {candidates:?}"
            );
        }

        #[test]
        fn debug_setup_script_candidates_start_with_the_checkout() {
            let cwd = checkout();
            let exe = app_exe();
            let candidates = setup_script_candidates("setup_zsh.sh", Some(&cwd), Some(&exe), true);

            assert_eq!(
                candidates.first(),
                Some(
                    &cwd.join("assets")
                        .join("shell-integration")
                        .join("setup_zsh.sh")
                ),
                "debug builds look in the checkout first: {candidates:?}"
            );
        }

        #[test]
        fn setup_script_lookup_includes_the_working_directory_only_in_debug_builds() {
            let source = include_str!("init.rs");
            let start = source
                .find("fn resolve_setup_script(")
                .expect("resolve_setup_script exists");
            let body = &source[start..];
            let body = &body[..body.find("\n    }\n").expect("end of resolve_setup_script")];

            assert!(
                body.contains("setup_script_candidates("),
                "resolve_setup_script builds its list through setup_script_candidates"
            );
            assert!(
                body.contains("cfg!(debug_assertions)"),
                "the working directory is gated on debug builds"
            );
        }
    }
}
