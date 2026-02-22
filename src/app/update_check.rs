// Claude Code Rust - A native Rust terminal interface for Claude Code
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

use super::App;
use crate::Cli;
use crate::acp::client::ClientEvent;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;

const UPDATE_CHECK_DISABLE_ENV: &str = "CLAUDE_RUST_NO_UPDATE_CHECK";
const UPDATE_CHECK_TTL_SECS: u64 = 24 * 60 * 60;
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(4);
const REPO_URL: &str = "https://github.com/srothgan/claude-code-rust";
const CACHE_FILE: &str = "update-check.json";
const CACHE_DIR_NAME: &str = "claude-code-rust";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SimpleVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateCheckCache {
    checked_at_unix_secs: u64,
    latest_version: String,
}

pub fn start_update_check(app: &App, cli: &Cli) {
    if update_check_disabled(cli.no_update_check) {
        tracing::debug!("Skipping update check (disabled by flag/env)");
        return;
    }

    let event_tx = app.event_tx.clone();
    let current_version = env!("CARGO_PKG_VERSION").to_owned();

    tokio::task::spawn_local(async move {
        let latest_version = resolve_latest_version().await;
        let Some(latest_version) = latest_version else {
            return;
        };

        if is_newer_version(&latest_version, &current_version) {
            let _ = event_tx.send(ClientEvent::UpdateAvailable { latest_version, current_version });
        }
    });
}

fn update_check_disabled(no_update_check_flag: bool) -> bool {
    if no_update_check_flag {
        return true;
    }
    std::env::var(UPDATE_CHECK_DISABLE_ENV)
        .ok()
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

async fn resolve_latest_version() -> Option<String> {
    let cache_path = update_cache_path()?;
    let now = unix_now_secs()?;
    let cached = read_cache(&cache_path).await;

    if let Some(cache) = cached.as_ref()
        && now.saturating_sub(cache.checked_at_unix_secs) <= UPDATE_CHECK_TTL_SECS
        && is_valid_version(&cache.latest_version)
    {
        return Some(cache.latest_version.clone());
    }

    match fetch_latest_repo_tag().await {
        Some(latest_version) => {
            let cache = UpdateCheckCache { checked_at_unix_secs: now, latest_version };
            if let Err(err) = write_cache(&cache_path, &cache).await {
                tracing::debug!("update-check cache write failed: {err}");
            }
            Some(cache.latest_version)
        }
        None => cached.and_then(|cache| {
            is_valid_version(&cache.latest_version).then_some(cache.latest_version)
        }),
    }
}

fn update_cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| dir.join(CACHE_DIR_NAME).join(CACHE_FILE))
}

fn unix_now_secs() -> Option<u64> {
    SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

async fn read_cache(path: &Path) -> Option<UpdateCheckCache> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str::<UpdateCheckCache>(&content).ok()
}

async fn write_cache(path: &Path, cache: &UpdateCheckCache) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let content = serde_json::to_vec(cache)?;
    tokio::fs::write(path, content).await?;
    Ok(())
}

async fn fetch_latest_repo_tag() -> Option<String> {
    let output = tokio::time::timeout(
        UPDATE_CHECK_TIMEOUT,
        Command::new("git").args(["ls-remote", "--tags", "--refs", REPO_URL]).output(),
    )
    .await
    .ok()?
    .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    latest_version_from_git_ls_remote(&stdout)
}

fn latest_version_from_git_ls_remote(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .filter_map(parse_ls_remote_tag_ref)
        .max()
        .map(|v| format!("{}.{}.{}", v.major, v.minor, v.patch))
}

fn parse_ls_remote_tag_ref(line: &str) -> Option<SimpleVersion> {
    let (_, ref_name) = line.split_once('\t')?;
    let tag = ref_name.strip_prefix("refs/tags/")?;
    parse_simple_version(tag)
}

fn parse_simple_version(raw: &str) -> Option<SimpleVersion> {
    let trimmed = raw.trim();
    let without_prefix = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let core = without_prefix.split_once('-').map_or(without_prefix, |(c, _)| c);

    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(SimpleVersion { major, minor, patch })
}

fn is_valid_version(version: &str) -> bool {
    parse_simple_version(version).is_some()
}

fn is_newer_version(candidate: &str, current: &str) -> bool {
    let Some(candidate) = parse_simple_version(candidate) else {
        return false;
    };
    let Some(current) = parse_simple_version(current) else {
        return false;
    };
    candidate > current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_version_accepts_v_prefix() {
        assert_eq!(
            parse_simple_version("v1.2.3"),
            Some(SimpleVersion { major: 1, minor: 2, patch: 3 })
        );
    }

    #[test]
    fn parse_simple_version_rejects_invalid_shapes() {
        assert_eq!(parse_simple_version("1.2"), None);
        assert_eq!(parse_simple_version("1.2.3.4"), None);
        assert_eq!(parse_simple_version("v1.two.3"), None);
    }

    #[test]
    fn parse_simple_version_ignores_prerelease_suffix() {
        assert_eq!(
            parse_simple_version("v2.4.6-rc1"),
            Some(SimpleVersion { major: 2, minor: 4, patch: 6 })
        );
    }

    #[test]
    fn latest_version_from_git_output_picks_highest_semver() {
        let output =
            ["aaa\trefs/tags/v0.2.0", "bbb\trefs/tags/v0.10.0", "ccc\trefs/tags/v0.9.9"].join("\n");
        assert_eq!(latest_version_from_git_ls_remote(&output).as_deref(), Some("0.10.0"));
    }

    #[test]
    fn update_check_disabled_prefers_flag() {
        assert!(update_check_disabled(true));
    }

    #[test]
    fn is_newer_version_compares_semver_triplets() {
        assert!(is_newer_version("0.3.0", "0.2.9"));
        assert!(!is_newer_version("0.2.9", "0.3.0"));
        assert!(!is_newer_version("bad", "0.3.0"));
    }
}
