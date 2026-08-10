//! Runtime path resolution for plugin-owned Tabby state.
//!
//! Runtime state is owned by one Herdr Session. This module only resolves the
//! plugin state directory and derives paths indexed by a validated session key.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::string::FromUtf8Error;

pub const PLUGIN_ID: &str = "yersonargotev.tabby";
pub const HERDR_PLUGIN_STATE_DIR_ENV: &str = "HERDR_PLUGIN_STATE_DIR";
pub const HERDR_PLUGIN_CONFIG_DIR_ENV: &str = "HERDR_PLUGIN_CONFIG_DIR";
pub const XDG_STATE_HOME_ENV: &str = "XDG_STATE_HOME";
pub const HOME_ENV: &str = "HOME";
const SESSION_TAB_STATE_DIR_NAME: &str = "session-tab-state";

/// Returns the persisted tab-state path owned by one validated Herdr Session.
///
/// The storage key is deliberately constrained to Tabby's lossless v2 SHA-256
/// format before it becomes part of a path. The persisted record still embeds
/// the original Session Identity; this path is only an index, never authority.
pub fn session_tab_state_path(
    state_base: impl AsRef<Path>,
    session_key: &str,
) -> Result<PathBuf, StatePathError> {
    let state_base = absolute_session_state_base(state_base.as_ref().to_path_buf())?;
    if !is_session_storage_key(session_key) {
        return Err(StatePathError::InvalidSessionStorageKey(
            session_key.to_string(),
        ));
    }
    Ok(state_base
        .join(SESSION_TAB_STATE_DIR_NAME)
        .join(session_key)
        .join("state.json"))
}

fn is_session_storage_key(key: &str) -> bool {
    let Some(digest) = key.strip_prefix("v2-") else {
        return false;
    };
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginStateDirInputs {
    pub herdr_plugin_state_dir: Option<OsString>,
    pub herdr_plugin_config_dir: Option<OsString>,
    pub xdg_state_home: Option<OsString>,
    pub home: Option<OsString>,
}

impl PluginStateDirInputs {
    pub fn from_env() -> Self {
        Self {
            herdr_plugin_state_dir: std::env::var_os(HERDR_PLUGIN_STATE_DIR_ENV),
            herdr_plugin_config_dir: std::env::var_os(HERDR_PLUGIN_CONFIG_DIR_ENV),
            xdg_state_home: std::env::var_os(XDG_STATE_HOME_ENV),
            home: std::env::var_os(HOME_ENV),
        }
    }
}

pub fn plugin_state_dir_from_inputs(
    inputs: &PluginStateDirInputs,
) -> Option<(PathBuf, PluginStateDirSource)> {
    if let Some(path) = inputs.herdr_plugin_state_dir.as_ref() {
        return Some((
            PathBuf::from(path),
            PluginStateDirSource::HerdrPluginStateDir,
        ));
    }

    if let Some(path) = inputs.herdr_plugin_config_dir.as_ref() {
        return Some((
            PathBuf::from(path),
            PluginStateDirSource::HerdrPluginConfigDir,
        ));
    }

    if let Some(path) = inputs
        .xdg_state_home
        .as_ref()
        .filter(|path| !path.is_empty())
    {
        return Some((
            PathBuf::from(path)
                .join("herdr")
                .join("plugins")
                .join(PLUGIN_ID),
            PluginStateDirSource::XdgStateHome,
        ));
    }

    inputs
        .home
        .as_ref()
        .filter(|path| !path.is_empty())
        .map(|path| {
            (
                PathBuf::from(path)
                    .join(".local")
                    .join("state")
                    .join("herdr")
                    .join("plugins")
                    .join(PLUGIN_ID),
                PluginStateDirSource::Home,
            )
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStateDirSource {
    HerdrPluginStateDir,
    HerdrPluginConfigDir,
    XdgStateHome,
    Home,
}

pub fn should_remove_stale_herdr_socket_path(socket_path: Option<&OsStr>) -> bool {
    let Some(socket_path) = socket_path.filter(|path| !path.is_empty()) else {
        return false;
    };

    let socket_path = Path::new(socket_path);
    socket_path.is_absolute() && !socket_path.exists()
}

pub fn herdr_plugin_config_dir(plugin_id: &str) -> Result<PathBuf, StatePathError> {
    let output = Command::new("herdr")
        .args(["plugin", "config-dir", plugin_id])
        .output()
        .map_err(StatePathError::HerdrConfigDirIo)?;

    if !output.status.success() {
        return Err(StatePathError::HerdrConfigDirFailed {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    let stdout = String::from_utf8(output.stdout)?;
    let path = stdout.trim();
    if path.is_empty() {
        return Err(StatePathError::EmptyHerdrPluginConfigDir);
    }

    Ok(PathBuf::from(path))
}

fn absolute_session_state_base(path: PathBuf) -> Result<PathBuf, StatePathError> {
    if path.as_os_str().is_empty() {
        return Err(StatePathError::EmptySessionStateBase);
    }

    if !path.is_absolute() {
        return Err(StatePathError::RelativeSessionStateBase(path));
    }

    Ok(path)
}

#[derive(Debug)]
pub enum StatePathError {
    EmptySessionStateBase,
    RelativeSessionStateBase(PathBuf),
    EmptyHerdrPluginConfigDir,
    HerdrConfigDirIo(std::io::Error),
    HerdrConfigDirFailed { status: ExitStatus, stderr: String },
    HerdrConfigDirUtf8(FromUtf8Error),
    InvalidSessionStorageKey(String),
}

impl fmt::Display for StatePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySessionStateBase => {
                write!(
                    formatter,
                    "session state base resolved an empty Tabby state path"
                )
            }
            Self::RelativeSessionStateBase(path) => write!(
                formatter,
                "session state base resolved relative Tabby state path `{}`; refusing to write plugin state outside an explicit absolute path",
                path.display()
            ),
            Self::EmptyHerdrPluginConfigDir => write!(
                formatter,
                "`herdr plugin config-dir {PLUGIN_ID}` returned an empty Tabby state directory"
            ),
            Self::HerdrConfigDirIo(error) => write!(
                formatter,
                "failed to run `herdr plugin config-dir {PLUGIN_ID}` for Tabby state path: {error}"
            ),
            Self::HerdrConfigDirFailed { status, stderr } => write!(
                formatter,
                "`herdr plugin config-dir {PLUGIN_ID}` failed with {status}: {stderr}"
            ),
            Self::HerdrConfigDirUtf8(error) => write!(
                formatter,
                "`herdr plugin config-dir {PLUGIN_ID}` returned non-UTF-8 output: {error}"
            ),
            Self::InvalidSessionStorageKey(key) => write!(
                formatter,
                "invalid Tabby Session Identity storage key `{key}`"
            ),
        }
    }
}

impl std::error::Error for StatePathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HerdrConfigDirIo(error) => Some(error),
            Self::HerdrConfigDirUtf8(error) => Some(error),
            Self::EmptySessionStateBase
            | Self::RelativeSessionStateBase(_)
            | Self::EmptyHerdrPluginConfigDir
            | Self::HerdrConfigDirFailed { .. }
            | Self::InvalidSessionStorageKey(_) => None,
        }
    }
}

impl From<FromUtf8Error> for StatePathError {
    fn from(error: FromUtf8Error) -> Self {
        Self::HerdrConfigDirUtf8(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_a_session_scoped_state_path_from_a_valid_session_key() {
        let path = session_tab_state_path(
            Path::new("/tmp/tabby-test/state"),
            "v2-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("resolve session state path");

        assert_eq!(
            path,
            PathBuf::from(
                "/tmp/tabby-test/state/session-tab-state/v2-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/state.json"
            )
        );
    }

    #[test]
    fn refuses_a_session_key_that_could_escape_the_state_directory() {
        let error = session_tab_state_path(Path::new("/tmp/tabby-test/state"), "../other")
            .expect_err("session key must be a derived storage key");

        assert!(matches!(error, StatePathError::InvalidSessionStorageKey(_)));
    }
}
