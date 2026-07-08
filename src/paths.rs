//! Runtime path resolution for plugin-owned Tabby state.
//!
//! `TABBY_LOCK_STORE_PATH` stays the explicit test/development override. When it
//! is absent, runtime commands ask Herdr for Tabby's plugin-owned config
//! directory instead of inventing a path under the user's home directory.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::string::FromUtf8Error;

pub const PLUGIN_ID: &str = "yersonargotev.tabby";
pub const LOCK_STORE_PATH_ENV: &str = "TABBY_LOCK_STORE_PATH";
pub const HERDR_PLUGIN_STATE_DIR_ENV: &str = "HERDR_PLUGIN_STATE_DIR";
pub const HERDR_PLUGIN_CONFIG_DIR_ENV: &str = "HERDR_PLUGIN_CONFIG_DIR";
const XDG_STATE_HOME_ENV: &str = "XDG_STATE_HOME";
const HOME_ENV: &str = "HOME";
const LOCK_STORE_FILE_NAME: &str = "locks.json";

pub fn lock_store_path_from_runtime() -> Result<PathBuf, StatePathError> {
    resolve_lock_store_path_with(RuntimePathInputs::from_env(), || {
        herdr_plugin_config_dir(PLUGIN_ID)
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimePathInputs {
    pub lock_store_override: Option<OsString>,
    pub herdr_plugin_state_dir: Option<OsString>,
    pub herdr_plugin_config_dir: Option<OsString>,
    pub xdg_state_home: Option<OsString>,
    pub home: Option<OsString>,
}

impl RuntimePathInputs {
    fn from_env() -> Self {
        Self {
            lock_store_override: std::env::var_os(LOCK_STORE_PATH_ENV),
            herdr_plugin_state_dir: std::env::var_os(HERDR_PLUGIN_STATE_DIR_ENV),
            herdr_plugin_config_dir: std::env::var_os(HERDR_PLUGIN_CONFIG_DIR_ENV),
            xdg_state_home: std::env::var_os(XDG_STATE_HOME_ENV),
            home: std::env::var_os(HOME_ENV),
        }
    }
}

pub fn resolve_lock_store_path_with(
    inputs: RuntimePathInputs,
    discover_plugin_config_dir: impl FnOnce() -> Result<PathBuf, StatePathError>,
) -> Result<PathBuf, StatePathError> {
    if let Some(path) = inputs.lock_store_override {
        return absolute_path(PathBuf::from(path), StatePathSource::Override);
    }

    if let Some(path) = inputs.herdr_plugin_state_dir {
        return state_file_in_dir(PathBuf::from(path), StatePathSource::HerdrPluginStateDir);
    }

    if let Some(path) = inputs.herdr_plugin_config_dir {
        return state_file_in_dir(PathBuf::from(path), StatePathSource::HerdrPluginConfigDir);
    }

    if let Some((path, source)) = default_plugin_state_dir(inputs.xdg_state_home, inputs.home) {
        return state_file_in_dir(path, source);
    }

    state_file_in_dir(
        discover_plugin_config_dir()?,
        StatePathSource::HerdrPluginConfigDirCommand,
    )
}

fn default_plugin_state_dir(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<(PathBuf, StatePathSource)> {
    if let Some(path) = xdg_state_home.filter(|path| !path.is_empty()) {
        return Some((
            PathBuf::from(path)
                .join("herdr")
                .join("plugins")
                .join(PLUGIN_ID),
            StatePathSource::XdgStateHome,
        ));
    }

    home.filter(|path| !path.is_empty()).map(|path| {
        (
            PathBuf::from(path)
                .join(".local")
                .join("state")
                .join("herdr")
                .join("plugins")
                .join(PLUGIN_ID),
            StatePathSource::Home,
        )
    })
}

fn herdr_plugin_config_dir(plugin_id: &str) -> Result<PathBuf, StatePathError> {
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
        return Err(StatePathError::EmptyPath {
            source: StatePathSource::HerdrPluginConfigDirCommand,
        });
    }

    Ok(PathBuf::from(path))
}

fn state_file_in_dir(dir: PathBuf, source: StatePathSource) -> Result<PathBuf, StatePathError> {
    let dir = absolute_path(dir, source)?;
    Ok(dir.join(LOCK_STORE_FILE_NAME))
}

fn absolute_path(path: PathBuf, source: StatePathSource) -> Result<PathBuf, StatePathError> {
    if path.as_os_str().is_empty() {
        return Err(StatePathError::EmptyPath { source });
    }

    if !path.is_absolute() {
        return Err(StatePathError::RelativePath { source, path });
    }

    Ok(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatePathSource {
    Override,
    HerdrPluginStateDir,
    HerdrPluginConfigDir,
    XdgStateHome,
    Home,
    HerdrPluginConfigDirCommand,
}

impl fmt::Display for StatePathSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Override => LOCK_STORE_PATH_ENV,
            Self::HerdrPluginStateDir => HERDR_PLUGIN_STATE_DIR_ENV,
            Self::HerdrPluginConfigDir => HERDR_PLUGIN_CONFIG_DIR_ENV,
            Self::XdgStateHome => XDG_STATE_HOME_ENV,
            Self::Home => HOME_ENV,
            Self::HerdrPluginConfigDirCommand => "herdr plugin config-dir",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug)]
pub enum StatePathError {
    EmptyPath {
        source: StatePathSource,
    },
    RelativePath {
        source: StatePathSource,
        path: PathBuf,
    },
    HerdrConfigDirIo(std::io::Error),
    HerdrConfigDirFailed {
        status: ExitStatus,
        stderr: String,
    },
    HerdrConfigDirUtf8(FromUtf8Error),
}

impl fmt::Display for StatePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath { source } => {
                write!(formatter, "{source} resolved an empty Tabby state path")
            }
            Self::RelativePath { source, path } => write!(
                formatter,
                "{source} resolved relative Tabby state path `{}`; refusing to write plugin state outside an explicit absolute path",
                path.display()
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
        }
    }
}

impl std::error::Error for StatePathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HerdrConfigDirIo(error) => Some(error),
            Self::HerdrConfigDirUtf8(error) => Some(error),
            Self::EmptyPath { .. }
            | Self::RelativePath { .. }
            | Self::HerdrConfigDirFailed { .. } => None,
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
    use std::ffi::OsString;

    #[test]
    fn override_path_wins_over_herdr_defaults() {
        let path = resolve_lock_store_path_with(
            RuntimePathInputs {
                lock_store_override: Some(OsString::from("/tmp/tabby-test/override.json")),
                herdr_plugin_state_dir: Some(OsString::from("/tmp/tabby-test/state")),
                herdr_plugin_config_dir: Some(OsString::from("/tmp/tabby-test/config")),
                ..RuntimePathInputs::default()
            },
            || panic!("override must not call Herdr config-dir"),
        )
        .expect("resolve override path");

        assert_eq!(path, PathBuf::from("/tmp/tabby-test/override.json"));
    }

    #[test]
    fn state_dir_env_wins_over_config_dir_env() {
        let path = resolve_lock_store_path_with(
            RuntimePathInputs {
                lock_store_override: None,
                herdr_plugin_state_dir: Some(OsString::from("/tmp/tabby-test/state")),
                herdr_plugin_config_dir: Some(OsString::from("/tmp/tabby-test/config")),
                ..RuntimePathInputs::default()
            },
            || panic!("env state dir must not call Herdr config-dir"),
        )
        .expect("resolve state dir path");

        assert_eq!(path, PathBuf::from("/tmp/tabby-test/state/locks.json"));
    }

    #[test]
    fn config_dir_env_is_used_when_state_dir_env_is_absent() {
        let path = resolve_lock_store_path_with(
            RuntimePathInputs {
                lock_store_override: None,
                herdr_plugin_state_dir: None,
                herdr_plugin_config_dir: Some(OsString::from("/tmp/tabby-test/config")),
                ..RuntimePathInputs::default()
            },
            || panic!("env config dir must not call Herdr config-dir"),
        )
        .expect("resolve config dir path");

        assert_eq!(path, PathBuf::from("/tmp/tabby-test/config/locks.json"));
    }

    #[test]
    fn herdr_config_dir_command_is_default_when_no_env_paths_exist() {
        let path = resolve_lock_store_path_with(RuntimePathInputs::default(), || {
            Ok(PathBuf::from("/tmp/tabby-test/herdr-config"))
        })
        .expect("resolve Herdr config-dir path");

        assert_eq!(
            path,
            PathBuf::from("/tmp/tabby-test/herdr-config/locks.json")
        );
    }

    #[test]
    fn xdg_state_home_matches_herdr_plugin_state_layout_without_plugin_env() {
        let path = resolve_lock_store_path_with(
            RuntimePathInputs {
                xdg_state_home: Some(OsString::from("/tmp/tabby-test/xdg-state")),
                home: Some(OsString::from("/tmp/tabby-test/home")),
                ..RuntimePathInputs::default()
            },
            || panic!("XDG_STATE_HOME should avoid Herdr config-dir discovery"),
        )
        .expect("resolve XDG state path");

        assert_eq!(
            path,
            PathBuf::from("/tmp/tabby-test/xdg-state/herdr/plugins/yersonargotev.tabby/locks.json")
        );
    }

    #[test]
    fn home_state_fallback_matches_herdr_plugin_state_layout_without_plugin_env() {
        let path = resolve_lock_store_path_with(
            RuntimePathInputs {
                home: Some(OsString::from("/tmp/tabby-test/home")),
                ..RuntimePathInputs::default()
            },
            || panic!("HOME state fallback should avoid Herdr config-dir discovery"),
        )
        .expect("resolve HOME state path");

        assert_eq!(
            path,
            PathBuf::from(
                "/tmp/tabby-test/home/.local/state/herdr/plugins/yersonargotev.tabby/locks.json"
            )
        );
    }

    #[test]
    fn refuses_relative_override_path() {
        let error = resolve_lock_store_path_with(
            RuntimePathInputs {
                lock_store_override: Some(OsString::from("relative/locks.json")),
                ..RuntimePathInputs::default()
            },
            || panic!("relative override must fail before discovery"),
        )
        .expect_err("relative override should be rejected");

        assert!(matches!(
            error,
            StatePathError::RelativePath {
                source: StatePathSource::Override,
                ..
            }
        ));
    }

    #[test]
    fn refuses_empty_default_config_dir() {
        let error =
            resolve_lock_store_path_with(RuntimePathInputs::default(), || Ok(PathBuf::new()))
                .expect_err("empty config dir should be rejected");

        assert!(matches!(
            error,
            StatePathError::EmptyPath {
                source: StatePathSource::HerdrPluginConfigDirCommand
            }
        ));
    }
}
