// claude_rust - A native Rust terminal interface for Claude Code
// Copyright (C) 2025  Simon Peter Rothgang
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use agent_client_protocol::{self as acp};
use anyhow::Context as _;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub const ADAPTER_NPM_PACKAGE: &str = "@zed-industries/claude-code-acp";
const ADAPTER_BIN_ENV: &str = "CLAUDE_CODE_ACP_BIN";
const GLOBAL_ADAPTER_BIN_CANDIDATES: [&str; 2] = ["claude-code-acp", "zed-claude-code-acp"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterLauncher {
    /// Launch a specific adapter binary path (CLI/env override).
    Direct(PathBuf),
    /// Launch a globally installed adapter binary discovered via PATH.
    Global(PathBuf),
    /// Launch via `npx @zed-industries/claude-code-acp` as the final fallback.
    Npx(PathBuf),
}

impl AdapterLauncher {
    #[must_use]
    pub fn command_path(&self) -> &Path {
        match self {
            Self::Direct(path) | Self::Global(path) | Self::Npx(path) => path,
        }
    }

    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Direct(_) => "direct",
            Self::Global(_) => "global",
            Self::Npx(_) => "npx",
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        format!("{} ({})", self.label(), self.command_path().display())
    }
}

/// Resolve all adapter launchers in priority order:
/// 1) `--adapter-bin`
/// 2) `CLAUDE_CODE_ACP_BIN`
/// 3) globally installed adapter binaries from PATH
/// 4) `npx @zed-industries/claude-code-acp`
pub fn resolve_adapter_launchers(
    cli_adapter_bin: Option<&Path>,
) -> anyhow::Result<Vec<AdapterLauncher>> {
    let env_adapter_bin =
        std::env::var_os(ADAPTER_BIN_ENV).filter(|v| !v.is_empty()).map(PathBuf::from);
    let global_bins = GLOBAL_ADAPTER_BIN_CANDIDATES
        .iter()
        .filter_map(|bin| which::which(bin).ok())
        .collect::<Vec<_>>();
    let npx_path = which::which("npx").ok();

    let launchers = build_adapter_launchers(
        cli_adapter_bin.map(Path::to_path_buf),
        env_adapter_bin,
        global_bins,
        npx_path,
    );

    if launchers.is_empty() {
        anyhow::bail!(
            "No ACP adapter launcher found. Set --adapter-bin, set {ADAPTER_BIN_ENV}, install a global \
             adapter binary, or install Node.js/npx for {ADAPTER_NPM_PACKAGE}."
        );
    }

    Ok(launchers)
}

fn push_unique_launcher(
    launchers: &mut Vec<AdapterLauncher>,
    seen_paths: &mut HashSet<PathBuf>,
    launcher: AdapterLauncher,
) {
    let path = launcher.command_path().to_path_buf();
    if seen_paths.insert(path) {
        launchers.push(launcher);
    }
}

fn build_adapter_launchers(
    cli_adapter_bin: Option<PathBuf>,
    env_adapter_bin: Option<PathBuf>,
    global_bins: Vec<PathBuf>,
    npx_path: Option<PathBuf>,
) -> Vec<AdapterLauncher> {
    let mut launchers = Vec::new();
    let mut seen_paths = HashSet::new();

    if let Some(path) = cli_adapter_bin {
        push_unique_launcher(&mut launchers, &mut seen_paths, AdapterLauncher::Direct(path));
    }
    if let Some(path) = env_adapter_bin {
        push_unique_launcher(&mut launchers, &mut seen_paths, AdapterLauncher::Direct(path));
    }
    for path in global_bins {
        push_unique_launcher(&mut launchers, &mut seen_paths, AdapterLauncher::Global(path));
    }
    if let Some(path) = npx_path {
        push_unique_launcher(&mut launchers, &mut seen_paths, AdapterLauncher::Npx(path));
    }

    launchers
}

pub struct AdapterProcess {
    pub child: Child,
    pub connection: acp::ClientSideConnection,
}

/// Spawn the ACP adapter as a child process and establish the connection.
///
/// `launcher` should be resolved once at startup and reused.
///
/// Must be called from within a `tokio::task::LocalSet` context because
/// ACP futures are `!Send`.
#[allow(clippy::unused_async)]
pub async fn spawn_adapter(
    client: impl acp::Client + 'static,
    launcher: &AdapterLauncher,
    cwd: &Path,
) -> anyhow::Result<AdapterProcess> {
    let mut command = match launcher {
        AdapterLauncher::Npx(npx_path) => {
            let mut command_builder = Command::new(npx_path);
            command_builder
                .arg(ADAPTER_NPM_PACKAGE)
                .env("NO_UPDATE_NOTIFIER", "1")
                .env("NPM_CONFIG_UPDATE_NOTIFIER", "false")
                .env("NPM_CONFIG_FUND", "false")
                .env("NPM_CONFIG_AUDIT", "false");
            command_builder
        }
        AdapterLauncher::Direct(path) | AdapterLauncher::Global(path) => Command::new(path),
    };

    let mut child = command
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to spawn adapter via {}", launcher.describe()))?;

    let child_stdin =
        child.stdin.take().ok_or_else(|| anyhow::anyhow!("Failed to capture adapter stdin"))?;
    let child_stdout =
        child.stdout.take().ok_or_else(|| anyhow::anyhow!("Failed to capture adapter stdout"))?;

    let stdin_compat = child_stdin.compat_write();
    let stdout_compat = child_stdout.compat();

    let (connection, io_future) =
        acp::ClientSideConnection::new(client, stdin_compat, stdout_compat, |fut| {
            tokio::task::spawn_local(fut);
        });

    // Spawn the I/O handler on the LocalSet
    tokio::task::spawn_local(async move {
        if let Err(e) = io_future.await {
            tracing::error!("ACP I/O error: {e}");
        }
    });

    Ok(AdapterProcess { child, connection })
}

#[cfg(test)]
mod tests {
    use super::{AdapterLauncher, build_adapter_launchers};
    use std::path::PathBuf;

    #[test]
    fn launcher_order_prefers_cli_then_env_then_global_then_npx() {
        let launchers = build_adapter_launchers(
            Some(PathBuf::from("C:/custom/adapter")),
            Some(PathBuf::from("C:/env/adapter")),
            vec![PathBuf::from("C:/global/adapter")],
            Some(PathBuf::from("C:/node/npx")),
        );

        assert_eq!(
            launchers,
            vec![
                AdapterLauncher::Direct(PathBuf::from("C:/custom/adapter")),
                AdapterLauncher::Direct(PathBuf::from("C:/env/adapter")),
                AdapterLauncher::Global(PathBuf::from("C:/global/adapter")),
                AdapterLauncher::Npx(PathBuf::from("C:/node/npx"))
            ]
        );
    }

    #[test]
    fn duplicate_paths_are_removed() {
        let launchers = build_adapter_launchers(
            Some(PathBuf::from("C:/same/adapter")),
            Some(PathBuf::from("C:/same/adapter")),
            vec![PathBuf::from("C:/same/adapter"), PathBuf::from("C:/global/adapter")],
            Some(PathBuf::from("C:/node/npx")),
        );

        assert_eq!(
            launchers,
            vec![
                AdapterLauncher::Direct(PathBuf::from("C:/same/adapter")),
                AdapterLauncher::Global(PathBuf::from("C:/global/adapter")),
                AdapterLauncher::Npx(PathBuf::from("C:/node/npx"))
            ]
        );
    }
}
