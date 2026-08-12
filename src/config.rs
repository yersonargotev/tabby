//! Versioned user configuration compiled into one validated Label Policy.

use crate::labeler::{LabelPolicy, LabelPresentation};
use crate::startup::SessionSocket;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u8 = 1;
pub const MIN_MAX_LENGTH: usize = 1;
pub const MAX_MAX_LENGTH: usize = 128;
pub const MIN_MAX_DISPLAY_WIDTH: usize = 1;
pub const MAX_MAX_DISPLAY_WIDTH: usize = 256;
pub const MIN_CWD_COMPONENTS: usize = 1;
pub const MAX_CWD_COMPONENTS: usize = 8;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u8,
    #[serde(default)]
    labels: LabelsConfig,
    #[serde(default)]
    commands: CommandsConfig,
    #[serde(default)]
    directories: DirectoriesConfig,
    #[serde(default)]
    profiles: BTreeMap<String, ProfileConfig>,
    #[serde(default)]
    session_selectors: Vec<SessionSelectorConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LabelsConfig {
    max_length: Option<usize>,
    max_display_width: Option<usize>,
    cwd_components: Option<usize>,
    prefixes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CommandsConfig {
    additional_significant: Vec<String>,
    additional_ignored: Vec<String>,
    runners: BTreeMap<String, Vec<String>>,
    aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DirectoriesConfig {
    aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProfileConfig {
    extends: Option<String>,
    labels: LabelsConfig,
    commands: CommandsConfig,
    directories: DirectoriesConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionSelectorConfig {
    profile: String,
    identity: Option<String>,
    identity_hex: Option<String>,
    named_session: Option<String>,
}

#[derive(Debug)]
pub struct LoadedConfig {
    policy: LabelPolicy,
    source: ConfigSource,
    selected_profile: Option<String>,
}

impl LoadedConfig {
    pub fn policy(&self) -> &LabelPolicy {
        &self.policy
    }

    pub fn into_policy(self) -> LabelPolicy {
        self.policy
    }

    pub fn source(&self) -> ConfigSource {
        self.source
    }

    pub fn selected_profile(&self) -> Option<&str> {
        self.selected_profile.as_deref()
    }

    pub fn policy_source(&self) -> String {
        self.selected_profile()
            .map(|profile| format!("profile:{profile}"))
            .unwrap_or_else(|| "global".to_string())
    }

    pub(crate) fn built_in_defaults() -> Self {
        Self {
            policy: LabelPolicy::default(),
            source: ConfigSource::BuiltInDefaults,
            selected_profile: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    BuiltInDefaults,
    File,
}

impl ConfigSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BuiltInDefaults => "built-in defaults",
            Self::File => "config.toml",
        }
    }
}

pub fn parse(contents: &str) -> Result<LoadedConfig, ConfigError> {
    parse_with_home(
        contents,
        std::env::var_os("HOME").as_deref().map(Path::new),
        None,
    )
}

pub fn parse_for_session(
    contents: &str,
    session: &SessionSocket,
) -> Result<LoadedConfig, ConfigError> {
    parse_with_home(
        contents,
        std::env::var_os("HOME").as_deref().map(Path::new),
        Some(session),
    )
}

fn parse_with_home(
    contents: &str,
    home: Option<&Path>,
    session: Option<&SessionSocket>,
) -> Result<LoadedConfig, ConfigError> {
    let config: ConfigFile = toml::from_str(contents).map_err(ConfigError::Toml)?;
    if config.version != SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedVersion(config.version));
    }
    validate_selectors(&config, session)?;
    for profile in config.profiles.keys() {
        validate_token(&format!("profiles.{profile}"), profile)?;
        let resolved = resolve_profile(&config.profiles, profile, &mut Vec::new())?;
        compile_policy(
            &resolved.labels,
            &resolved.commands,
            &resolved.directories,
            home,
        )
        .map_err(|error| prefix_profile_error(error, profile))?;
    }
    let selected = match session {
        Some(session) => selected_profile_name(&config, session)?,
        None => None,
    };
    let (labels, commands, directories, selected_profile) = match selected {
        Some(profile) => {
            let resolved = resolve_profile(&config.profiles, &profile, &mut Vec::new())?;
            (
                resolved.labels,
                resolved.commands,
                resolved.directories,
                Some(profile),
            )
        }
        None => (
            config.labels.clone(),
            config.commands.clone(),
            config.directories.clone(),
            None,
        ),
    };
    let policy = compile_policy(&labels, &commands, &directories, home)?;
    Ok(LoadedConfig {
        policy,
        source: ConfigSource::File,
        selected_profile,
    })
}

pub fn load(path: &Path) -> Result<LoadedConfig, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) => parse(&contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(LoadedConfig::built_in_defaults())
        }
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn load_for_session(path: &Path, session: &SessionSocket) -> Result<LoadedConfig, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) => parse_for_session(&contents, session),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(LoadedConfig::built_in_defaults())
        }
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn path_from_env() -> Result<PathBuf, ConfigError> {
    let directory =
        if let Some(directory) = std::env::var_os(crate::paths::HERDR_PLUGIN_CONFIG_DIR_ENV) {
            PathBuf::from(directory)
        } else {
            crate::paths::herdr_plugin_config_dir(crate::paths::PLUGIN_ID)?
        };
    Ok(path_in_config_dir(&directory))
}

pub fn path_in_config_dir(directory: &Path) -> PathBuf {
    directory.join("config.toml")
}

pub fn check_from_env() -> Result<String, ConfigError> {
    let path = path_from_env()?;
    let loaded = load(&path)?;
    Ok(format!(
        "Tabby configuration is valid: {} (schema version {}, source {})",
        path.display(),
        SCHEMA_VERSION,
        loaded.source().as_str()
    ))
}

#[derive(Debug)]
pub enum ConfigError {
    Toml(toml::de::Error),
    UnsupportedVersion(u8),
    Validation { field: String, reason: String },
    Io { path: PathBuf, source: io::Error },
    Path(crate::paths::StatePathError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(error) => write!(formatter, "config.toml could not be parsed: {error}"),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "field `version` has unsupported value {version}; expected {SCHEMA_VERSION}"
            ),
            Self::Validation { field, reason } => {
                write!(formatter, "field `{field}` is invalid: {reason}")
            }
            Self::Io { path, source } => {
                write!(formatter, "could not read `{}`: {source}", path.display())
            }
            Self::Path(error) => write!(formatter, "could not resolve config.toml path: {error}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Toml(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::Path(error) => Some(error),
            Self::UnsupportedVersion(_) | Self::Validation { .. } => None,
        }
    }
}

impl From<crate::paths::StatePathError> for ConfigError {
    fn from(error: crate::paths::StatePathError) -> Self {
        Self::Path(error)
    }
}

fn compile_policy(
    labels: &LabelsConfig,
    commands: &CommandsConfig,
    directories: &DirectoriesConfig,
    home: Option<&Path>,
) -> Result<LabelPolicy, ConfigError> {
    validate_range(
        "labels.max_length",
        labels.max_length.unwrap_or(32),
        MIN_MAX_LENGTH,
        MAX_MAX_LENGTH,
    )?;
    if let Some(max_display_width) = labels.max_display_width {
        validate_range(
            "labels.max_display_width",
            max_display_width,
            MIN_MAX_DISPLAY_WIDTH,
            MAX_MAX_DISPLAY_WIDTH,
        )?;
    }
    validate_range(
        "labels.cwd_components",
        labels.cwd_components.unwrap_or(1),
        MIN_CWD_COMPONENTS,
        MAX_CWD_COMPONENTS,
    )?;
    validate_unique_tokens(
        "commands.additional_significant",
        &commands.additional_significant,
    )?;
    validate_unique_tokens("commands.additional_ignored", &commands.additional_ignored)?;
    let ignored = commands.additional_ignored.iter().collect::<BTreeSet<_>>();
    if let Some(command) = commands
        .additional_significant
        .iter()
        .find(|command| ignored.contains(command))
    {
        return validation_error(
            "commands.additional_significant",
            format!("`{command}` is also listed in commands.additional_ignored"),
        );
    }
    if let Some(command) = commands
        .additional_significant
        .iter()
        .find(|command| LabelPolicy::is_builtin_ignored_command(command))
    {
        return validation_error(
            "commands.additional_significant",
            format!("`{command}` is ignored by the built-in Label Policy"),
        );
    }
    if let Some(command) = commands
        .additional_significant
        .iter()
        .find(|command| LabelPolicy::is_builtin_significant_command(command))
    {
        return validation_error(
            "commands.additional_significant",
            format!("`{command}` duplicates a built-in Significant Command"),
        );
    }
    for (runner, subcommands) in &commands.runners {
        validate_token("commands.runners", runner)?;
        validate_unique_tokens(&format!("commands.runners.{runner}"), subcommands)?;
    }
    for (key, value) in &commands.aliases {
        validate_label_text("commands.aliases", key)?;
        validate_label_text(&format!("commands.aliases.{key}"), value)?;
    }
    validate_prefixes(labels, commands)?;
    let directory_aliases = normalize_directory_aliases(&directories.aliases, home)?;
    let runners = commands.runners.iter().flat_map(|(runner, subcommands)| {
        subcommands
            .iter()
            .map(move |subcommand| (runner.clone(), subcommand.clone()))
    });
    Ok(LabelPolicy::configured(
        commands.additional_significant.clone(),
        commands.additional_ignored.clone(),
        runners,
        LabelPresentation {
            aliases: commands.aliases.clone(),
            directory_aliases,
            prefixes: labels.prefixes.clone(),
            max_length: labels.max_length.unwrap_or(32),
            max_display_width: labels.max_display_width,
            cwd_components: labels.cwd_components.unwrap_or(1),
        },
    ))
}

fn validate_selectors(
    config: &ConfigFile,
    session: Option<&SessionSocket>,
) -> Result<(), ConfigError> {
    let mut resolved = BTreeSet::new();
    let mut named = BTreeSet::new();
    for (index, selector) in config.session_selectors.iter().enumerate() {
        let field = format!("session_selectors[{index}]");
        validate_token(&format!("{field}.profile"), &selector.profile)?;
        if !config.profiles.contains_key(&selector.profile) {
            return validation_error(
                &format!("{field}.profile"),
                format!("references unknown profile `{}`", selector.profile),
            );
        }
        let selector_count = usize::from(selector.identity.is_some())
            + usize::from(selector.identity_hex.is_some())
            + usize::from(selector.named_session.is_some());
        if selector_count != 1 {
            return validation_error(
                &field,
                "must set exactly one of identity, identity_hex, or named_session".to_string(),
            );
        }
        match (
            &selector.identity,
            &selector.identity_hex,
            &selector.named_session,
        ) {
            (Some(identity), None, None) => {
                if !Path::new(identity).is_absolute() {
                    return validation_error(
                        &format!("{field}.identity"),
                        "must be an absolute Herdr socket path".to_string(),
                    );
                }
                let socket =
                    SessionSocket::resolve(identity).map_err(|error| ConfigError::Validation {
                        field: format!("{field}.identity"),
                        reason: error.to_string(),
                    })?;
                if !resolved.insert(socket.identity_hex()) {
                    return validation_error(
                        &field,
                        "duplicates another resolved Session Identity".to_string(),
                    );
                }
            }
            (None, Some(identity_hex), None) => {
                validate_identity_hex(&format!("{field}.identity_hex"), identity_hex)?;
                if !resolved.insert(identity_hex.clone()) {
                    return validation_error(
                        &field,
                        "duplicates another resolved Session Identity".to_string(),
                    );
                }
            }
            (None, None, Some(name)) => {
                let mut components = Path::new(name).components();
                if !matches!(components.next(), Some(std::path::Component::Normal(_)))
                    || components.next().is_some()
                {
                    return validation_error(
                        &format!("{field}.named_session"),
                        "must be one non-empty session path component".to_string(),
                    );
                }
                if let Some(session) = session {
                    let socket = named_session_socket(session, name).ok_or_else(|| {
                        ConfigError::Validation {
                            field: format!("{field}.named_session"),
                            reason: "cannot derive a Herdr session root from this custom socket; use identity or identity_hex".to_string(),
                        }
                    })?;
                    if !resolved.insert(socket.identity_hex()) {
                        return validation_error(
                            &field,
                            "duplicates another resolved Session Identity".to_string(),
                        );
                    }
                } else if !named.insert(name.clone()) {
                    return validation_error(
                        &field,
                        "duplicates another named_session selector".to_string(),
                    );
                }
            }
            _ => unreachable!("selector count was validated"),
        }
    }
    Ok(())
}

fn selected_profile_name(
    config: &ConfigFile,
    session: &SessionSocket,
) -> Result<Option<String>, ConfigError> {
    for selector in &config.session_selectors {
        let selector_identity = match (
            &selector.identity,
            &selector.identity_hex,
            &selector.named_session,
        ) {
            (Some(identity), None, None) => SessionSocket::resolve(identity)
                .map_err(|error| ConfigError::Validation {
                    field: "session_selectors.identity".to_string(),
                    reason: error.to_string(),
                })?
                .identity_hex(),
            (None, Some(identity_hex), None) => identity_hex.clone(),
            (None, None, Some(name)) => named_session_socket(session, name)
                .ok_or_else(|| ConfigError::Validation {
                    field: "session_selectors.named_session".to_string(),
                    reason: "cannot derive a Herdr session root from this custom socket; use identity or identity_hex".to_string(),
                })?
                .identity_hex(),
            _ => continue,
        };
        if selector_identity == session.identity_hex() {
            return Ok(Some(selector.profile.clone()));
        }
    }
    Ok(None)
}

fn named_session_socket(session: &SessionSocket, name: &str) -> Option<SessionSocket> {
    let path = &session.identity_path;
    if path.file_name()?.to_str()? != "herdr.sock" {
        return None;
    }
    let parent = path.parent()?;
    let root = if parent.parent()?.file_name()?.to_str() == Some("sessions") {
        parent.parent()?.parent()?
    } else {
        parent
    };
    SessionSocket::resolve(root.join("sessions").join(name).join("herdr.sock")).ok()
}

fn resolve_profile(
    profiles: &BTreeMap<String, ProfileConfig>,
    name: &str,
    chain: &mut Vec<String>,
) -> Result<ProfileConfig, ConfigError> {
    if let Some(position) = chain.iter().position(|entry| entry == name) {
        let mut cycle = chain[position..].to_vec();
        cycle.push(name.to_string());
        return validation_error(
            &format!("profiles.{name}.extends"),
            format!("inheritance cycle: {}", cycle.join(" -> ")),
        );
    }
    let profile = profiles.get(name).ok_or_else(|| ConfigError::Validation {
        field: "profiles".to_string(),
        reason: format!("unknown profile `{name}`"),
    })?;
    chain.push(name.to_string());
    let resolved = match &profile.extends {
        Some(parent) => {
            if !profiles.contains_key(parent) {
                return validation_error(
                    &format!("profiles.{name}.extends"),
                    format!("references unknown profile `{parent}`"),
                );
            }
            merge_profiles(resolve_profile(profiles, parent, chain)?, profile, name)?
        }
        None => profile.clone(),
    };
    chain.pop();
    Ok(resolved)
}

fn merge_profiles(
    mut parent: ProfileConfig,
    child: &ProfileConfig,
    child_name: &str,
) -> Result<ProfileConfig, ConfigError> {
    parent.extends = child.extends.clone();
    parent.labels.max_length = child.labels.max_length.or(parent.labels.max_length);
    parent.labels.max_display_width = child
        .labels
        .max_display_width
        .or(parent.labels.max_display_width);
    parent.labels.cwd_components = child.labels.cwd_components.or(parent.labels.cwd_components);
    merge_map(
        &mut parent.labels.prefixes,
        &child.labels.prefixes,
        &format!("profiles.{child_name}.labels.prefixes"),
    )?;
    parent
        .commands
        .additional_significant
        .extend(child.commands.additional_significant.clone());
    parent
        .commands
        .additional_ignored
        .extend(child.commands.additional_ignored.clone());
    merge_map(
        &mut parent.commands.runners,
        &child.commands.runners,
        &format!("profiles.{child_name}.commands.runners"),
    )?;
    merge_map(
        &mut parent.commands.aliases,
        &child.commands.aliases,
        &format!("profiles.{child_name}.commands.aliases"),
    )?;
    merge_map(
        &mut parent.directories.aliases,
        &child.directories.aliases,
        &format!("profiles.{child_name}.directories.aliases"),
    )?;
    Ok(parent)
}

fn merge_map<T: Clone>(
    parent: &mut BTreeMap<String, T>,
    child: &BTreeMap<String, T>,
    field: &str,
) -> Result<(), ConfigError> {
    for (key, value) in child {
        if parent.contains_key(key) {
            return validation_error(field, format!("duplicates inherited key `{key}`"));
        }
        parent.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn validate_prefixes(labels: &LabelsConfig, commands: &CommandsConfig) -> Result<(), ConfigError> {
    let classified_candidates = LabelPolicy::classified_candidates(
        commands.additional_significant.iter().map(String::as_str),
        commands.runners.iter().flat_map(|(runner, subcommands)| {
            subcommands
                .iter()
                .map(move |subcommand| (runner.as_str(), subcommand.as_str()))
        }),
    );
    for (candidate, prefix) in &labels.prefixes {
        let field = format!("labels.prefixes.{candidate}");
        validate_label_text(&field, prefix)?;
        if !classified_candidates.contains(candidate) {
            return validation_error(
                &field,
                "must name a configured Significant Command or runner/subcommand candidate"
                    .to_string(),
            );
        }
        let command = candidate
            .split_once(' ')
            .map_or(candidate.as_str(), |(command, _)| command);
        if commands
            .additional_ignored
            .iter()
            .any(|ignored| ignored == command)
        {
            return validation_error(
                &field,
                "contradicts commands.additional_ignored and can never be presented".to_string(),
            );
        }
    }
    Ok(())
}

fn normalize_directory_aliases(
    aliases: &BTreeMap<String, String>,
    home: Option<&Path>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let mut normalized_aliases = BTreeMap::new();
    for (selector, alias) in aliases {
        let field = format!("directories.aliases.{selector}");
        validate_label_text(&field, alias)?;
        let path = if selector == "~" {
            home.map(Path::to_path_buf)
                .ok_or_else(|| ConfigError::Validation {
                    field: field.clone(),
                    reason: "uses `~` but HOME is not set".to_string(),
                })?
        } else if let Some(relative) = selector.strip_prefix("~/") {
            home.map(|home| home.join(relative))
                .ok_or_else(|| ConfigError::Validation {
                    field: field.clone(),
                    reason: "uses `~/` but HOME is not set".to_string(),
                })?
        } else {
            PathBuf::from(selector)
        };
        let normalized = crate::labeler::normalize_absolute_path(&path).ok_or_else(|| {
            ConfigError::Validation {
                field: field.clone(),
                reason: "must be an absolute path or start with `~/`".to_string(),
            }
        })?;
        if normalized_aliases
            .insert(normalized.clone(), alias.clone())
            .is_some()
        {
            return validation_error(
                &field,
                format!("duplicates normalized directory selector `{normalized}`"),
            );
        }
    }
    Ok(normalized_aliases)
}

fn validate_range(
    field: &str,
    value: usize,
    minimum: usize,
    maximum: usize,
) -> Result<(), ConfigError> {
    if !(minimum..=maximum).contains(&value) {
        return validation_error(field, format!("must be between {minimum} and {maximum}"));
    }
    Ok(())
}

fn validate_unique_tokens(field: &str, values: &[String]) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();
    for value in values {
        validate_token(field, value)?;
        if !seen.insert(value) {
            return validation_error(field, format!("contains duplicate entry `{value}`"));
        }
    }
    Ok(())
}

fn validate_token(field: &str, value: &str) -> Result<(), ConfigError> {
    validate_label_text(field, value)?;
    if value.chars().any(char::is_whitespace) {
        return validation_error(field, format!("`{value}` must be one command token"));
    }
    Ok(())
}

fn validate_identity_hex(field: &str, value: &str) -> Result<(), ConfigError> {
    if value.is_empty()
        || !value.len().is_multiple_of(2)
        || !value.starts_with("2f")
        || value.as_bytes().chunks_exact(2).any(|pair| pair == b"00")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return validation_error(
            field,
            "must encode an absolute, NUL-free Session Identity as even-length lowercase hexadecimal bytes".to_string(),
        );
    }
    Ok(())
}

fn validate_label_text(field: &str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return validation_error(field, "must not be empty".to_string());
    }
    if value.chars().any(char::is_control) {
        return validation_error(
            field,
            "contains a control character that cannot be represented safely in a Herdr tab label"
                .to_string(),
        );
    }
    Ok(())
}

fn validation_error<T>(field: &str, reason: String) -> Result<T, ConfigError> {
    Err(ConfigError::Validation {
        field: field.to_string(),
        reason,
    })
}

fn prefix_profile_error(error: ConfigError, profile: &str) -> ConfigError {
    match error {
        ConfigError::Validation { field, reason } => ConfigError::Validation {
            field: format!("profiles.{profile}.{field}"),
            reason,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr_client::{PaneInfo, PaneProcess, PaneProcessInfo};

    #[test]
    fn version_only_configuration_preserves_builtin_policy_behavior() {
        let loaded = parse("version = 1\n").expect("valid configuration");
        let pane = pane_with_cwd("tabby");

        let candidate = loaded
            .policy()
            .candidate_for_pane(&pane, None)
            .expect("Working Directory Suffix");

        assert_eq!(candidate.label(), "tabby");
    }

    #[test]
    fn schema_fields_compile_into_the_required_label_precedence() {
        let loaded = parse(
            r#"
version = 1

[labels]
max_length = 4
cwd_components = 2

[commands]
additional_significant = ["btop"]
additional_ignored = ["lazygit"]

[commands.runners]
pnpm = ["lint"]

[commands.aliases]
"btop" = "📊dash"
"pnpm lint" = "linting"
"unknown" = "never"
"#,
        )
        .expect("valid configuration");

        assert_eq!(candidate(&loaded, "btop", &["btop"]), "📊das");
        assert_eq!(candidate(&loaded, "pnpm", &["pnpm", "lint"]), "lint");
        assert_eq!(candidate(&loaded, "lazygit", &["lazygit"]), "dev/");
        assert_eq!(candidate(&loaded, "unknown", &["unknown"]), "dev/");
    }

    #[test]
    fn user_ignored_runner_from_node_shim_falls_back_to_working_directory_suffix() {
        let loaded = parse("version = 1\n[commands]\nadditional_ignored = [\"pnpm\"]\n")
            .expect("valid configuration");

        assert_eq!(
            candidate(
                &loaded,
                "node",
                &[
                    "node",
                    "/Users/me/Library/pnpm/.tools/pnpm/11.1.2/node_modules/pnpm/bin/pnpm.mjs",
                    "dev",
                ],
            ),
            "tabby"
        );
    }

    #[test]
    fn directory_aliases_replace_only_the_normalized_working_directory_fallback() {
        let loaded = parse_with_home(
            r#"
version = 1

[labels]
max_length = 4

[directories.aliases]
"/Users/me/code/./tabby" = "repository"
"~/code/notes" = "notes"
"/Users/me/code/linked" = "linked"
"/Users/me/code/real" = "real"
"/Users/me/code/deleted-path" = "gone"
"#,
            Some(Path::new("/Users/me")),
            None,
        )
        .expect("valid directory aliases");

        let mut pane = pane_with_cwd("unrelated");
        pane.cwd = Some("/Users/me/code/tabby/../tabby".to_string());
        assert_eq!(label_for(&loaded, &pane, None), "repo");

        pane.foreground_cwd = Some("/Users/me/code/notes".to_string());
        assert_eq!(label_for(&loaded, &pane, None), "note");

        pane.foreground_cwd = Some("/Users/me/code/linked".to_string());
        assert_eq!(label_for(&loaded, &pane, None), "link");
        pane.foreground_cwd = Some("/Users/me/code/real".to_string());
        assert_eq!(label_for(&loaded, &pane, None), "real");

        pane.foreground_cwd = None;
        pane.cwd = Some("/Users/me/code/deleted-path".to_string());
        assert_eq!(label_for(&loaded, &pane, None), "gone");

        pane.cwd = Some("/Users/me/code/tabby".to_string());
        assert_eq!(
            label_for(&loaded, &pane, Some(&process("nvim", &["nvim"]))),
            "nvim"
        );
    }

    #[test]
    fn directory_alias_validation_reports_the_specific_field() {
        for (contents, home, field) in [
            (
                "version = 1\n[directories.aliases]\nrelative = \"alias\"",
                Some(Path::new("/Users/me")),
                "directories.aliases.relative",
            ),
            (
                "version = 1\n[directories.aliases]\n\"~/code\" = \"alias\"",
                None,
                "directories.aliases.~/code",
            ),
            (
                "version = 1\n[directories.aliases]\n\"/code\" = \"\"",
                Some(Path::new("/Users/me")),
                "directories.aliases./code",
            ),
            (
                "version = 1\n[directories.aliases]\n\"/code/./tabby\" = \"one\"\n\"/code/tabby\" = \"two\"",
                Some(Path::new("/Users/me")),
                "directories.aliases./code/tabby",
            ),
            (
                "version = 1\n[directories.aliases]\n\"/code\" = \"bad\\u0000label\"",
                Some(Path::new("/Users/me")),
                "directories.aliases./code",
            ),
        ] {
            let error = parse_with_home(contents, home, None).expect_err("invalid directory alias");
            assert!(
                error.to_string().contains(field),
                "diagnostic `{error}` did not identify `{field}`"
            );
        }
    }

    #[test]
    fn selects_exact_profiles_per_session_and_uses_global_fallback() {
        let first = SessionSocket::resolve("/tmp/tabby-profile-first/herdr.sock").expect("first");
        let second =
            SessionSocket::resolve("/tmp/tabby-profile-second/herdr.sock").expect("second");
        let unmatched =
            SessionSocket::resolve("/tmp/tabby-profile-unmatched/herdr.sock").expect("unmatched");
        let config = r#"
version = 1

[labels]
cwd_components = 2

[profiles.work.labels]
cwd_components = 1

[profiles.personal.labels]
cwd_components = 3

[[session_selectors]]
profile = "work"
identity = "/tmp/tabby-profile-first/herdr.sock"

[[session_selectors]]
profile = "personal"
identity = "/tmp/tabby-profile-second/herdr.sock"
"#;

        let selected = parse_with_home(config, Some(Path::new("/Users/me")), Some(&first))
            .expect("selected profile");
        let selected_second = parse_with_home(config, Some(Path::new("/Users/me")), Some(&second))
            .expect("second selected profile");
        let restored = parse_with_home(config, Some(Path::new("/Users/me")), Some(&first))
            .expect("restored session selection");
        let fallback = parse_with_home(config, Some(Path::new("/Users/me")), Some(&unmatched))
            .expect("global fallback");

        assert_eq!(selected.selected_profile(), Some("work"));
        assert_eq!(selected.policy_source(), "profile:work");
        assert_eq!(label_for(&selected, &pane_with_cwd("tabby"), None), "tabby");
        assert_eq!(selected_second.selected_profile(), Some("personal"));
        assert_eq!(selected_second.policy_source(), "profile:personal");
        assert_eq!(
            label_for(&selected_second, &pane_with_cwd("tabby"), None),
            "me/dev/tabby"
        );
        assert_eq!(restored.selected_profile(), Some("work"));
        assert_eq!(fallback.selected_profile(), None);
        assert_eq!(fallback.policy_source(), "global");
    }

    #[test]
    fn named_session_shorthand_uses_the_receivers_documented_socket_root() {
        let receiver = SessionSocket::resolve("/tmp/herdr-root/herdr.sock").expect("receiver");
        let named =
            SessionSocket::resolve("/tmp/herdr-root/sessions/work/herdr.sock").expect("named");
        let config = r#"
version = 1
[profiles.work.labels]
cwd_components = 2
[[session_selectors]]
profile = "work"
named_session = "work"
"#;

        let selected = parse_with_home(config, None, Some(&named)).expect("named selection");
        let fallback = parse_with_home(config, None, Some(&receiver)).expect("global fallback");

        assert_eq!(selected.selected_profile(), Some("work"));
        assert_eq!(fallback.selected_profile(), None);
    }

    #[cfg(unix)]
    #[test]
    fn identity_hex_selects_a_non_utf8_lossless_session_identity() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let socket = SessionSocket::resolve(PathBuf::from(OsString::from_vec(
            b"/tmp/tabby-profile-\x80.sock".to_vec(),
        )))
        .expect("non-UTF-8 session");
        let config = format!(
            "version = 1\n[profiles.lossless.labels]\ncwd_components = 2\n[[session_selectors]]\nprofile = \"lossless\"\nidentity_hex = \"{}\"\n",
            socket.identity_hex()
        );

        let selected = parse_with_home(&config, None, Some(&socket)).expect("lossless selection");

        assert_eq!(selected.selected_profile(), Some("lossless"));
        assert_eq!(selected.policy_source(), "profile:lossless");
    }

    #[test]
    fn profiles_inherit_only_profiles_and_reject_ambiguous_configuration() {
        let socket = SessionSocket::resolve("/tmp/tabby-profile/herdr.sock").expect("socket");
        let valid = r#"
version = 1
[labels]
cwd_components = 2
[profiles.parent.commands]
additional_significant = ["yazi"]
[profiles.child]
extends = "parent"
[profiles.child.labels]
cwd_components = 1
[[session_selectors]]
profile = "child"
identity = "/tmp/tabby-profile/herdr.sock"
"#;
        let loaded = parse_with_home(valid, None, Some(&socket)).expect("inherited profile");
        assert_eq!(candidate(&loaded, "yazi", &["yazi"]), "yazi");

        for (contents, field) in [
            (
                "version = 1\n[profiles.a]\nextends = \"missing\"",
                "profiles.a.extends",
            ),
            (
                "version = 1\n[profiles.a]\nextends = \"b\"\n[profiles.b]\nextends = \"a\"",
                "profiles.a.extends",
            ),
            (
                "version = 1\n[profiles.a.commands.aliases]\nnvim = \"parent\"\n[profiles.b]\nextends = \"a\"\n[profiles.b.commands.aliases]\nnvim = \"child\"",
                "profiles.b.commands.aliases",
            ),
            (
                "version = 1\n[profiles.a]\n[[session_selectors]]\nprofile = \"a\"\nidentity = \"/tmp/a.sock\"\nnamed_session = \"a\"",
                "session_selectors[0]",
            ),
            (
                "version = 1\n[profiles.a]\n[[session_selectors]]\nprofile = \"a\"\nidentity_hex = \"ABC\"",
                "session_selectors[0].identity_hex",
            ),
            (
                "version = 1\n[profiles.a]\n[[session_selectors]]\nprofile = \"a\"\nnamed_session = \"..\"",
                "session_selectors[0].named_session",
            ),
            (
                "version = 1\n[profiles.a.labels]\nmax_length = 0",
                "profiles.a.labels.max_length",
            ),
        ] {
            let error =
                parse_with_home(contents, None, Some(&socket)).expect_err("invalid profile config");
            assert!(
                error.to_string().contains(field),
                "unexpected diagnostic: {error}"
            );
        }
    }

    #[test]
    fn rejects_each_invalid_schema_category_with_a_field_diagnostic() {
        for (contents, field) in [
            ("version = 2", "version"),
            ("version = 1\nunknown = true", "unknown"),
            ("version = 1\n[labels]\nmax_length = 0", "labels.max_length"),
            (
                "version = 1\n[labels]\ncwd_components = 9",
                "labels.cwd_components",
            ),
            (
                "version = 1\n[commands]\nadditional_significant = [\"btop\", \"btop\"]",
                "commands.additional_significant",
            ),
            (
                "version = 1\n[commands]\nadditional_significant = [\"python\"]\nadditional_ignored = [\"python\"]",
                "commands.additional_significant",
            ),
            (
                "version = 1\n[commands.runners]\n\"\" = [\"test\"]",
                "commands.runners",
            ),
            (
                "version = 1\n[commands.runners]\ncargo = [\"\"]",
                "commands.runners.cargo",
            ),
            (
                "version = 1\n[commands.aliases]\n\"\" = \"git\"",
                "commands.aliases",
            ),
            (
                "version = 1\n[commands.aliases]\ngit = \"\"",
                "commands.aliases.git",
            ),
            (
                "version = 1\n[commands.aliases]\ngit = \"bad\\u0000label\"",
                "commands.aliases.git",
            ),
            ("version = 1\n[directories]\nunknown = true", "unknown"),
        ] {
            let error = parse(contents).expect_err("configuration must be rejected");
            assert!(
                error.to_string().contains(field),
                "diagnostic `{error}` did not identify `{field}`"
            );
        }
    }

    #[test]
    fn rejects_additional_significant_command_that_is_ignored_by_default() {
        let error = parse("version = 1\n[commands]\nadditional_significant = [\"zsh\"]\n")
            .expect_err("configuration must reject an effective policy contradiction");

        assert_eq!(
            error.to_string(),
            "field `commands.additional_significant` is invalid: `zsh` is ignored by the built-in Label Policy"
        );
    }

    #[test]
    fn rejects_additional_significant_command_that_duplicates_a_default() {
        let error = parse("version = 1\n[commands]\nadditional_significant = [\"nvim\"]\n")
            .expect_err("configuration must reject an effective policy duplicate");

        assert_eq!(
            error.to_string(),
            "field `commands.additional_significant` is invalid: `nvim` duplicates a built-in Significant Command"
        );
    }

    #[test]
    fn missing_file_loads_defaults_without_creating_the_path() {
        let directory =
            std::env::temp_dir().join(format!("tabby-config-missing-{}", std::process::id()));
        let path = path_in_config_dir(&directory);

        let loaded = load(&path).expect("missing config uses defaults");

        assert_eq!(loaded.source(), ConfigSource::BuiltInDefaults);
        assert!(!path.exists());
        assert!(!directory.exists());
    }

    #[test]
    fn configuration_path_is_exactly_config_toml_under_the_herdr_directory() {
        assert_eq!(
            path_in_config_dir(Path::new("/tmp/herdr/plugin")),
            PathBuf::from("/tmp/herdr/plugin/config.toml")
        );
    }

    #[test]
    fn rejected_reload_keeps_the_last_valid_policy() {
        let directory = std::env::temp_dir().join(format!(
            "tabby-config-reload-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&directory).expect("config directory");
        let path = path_in_config_dir(&directory);
        let active =
            parse("version = 1\n[directories.aliases]\n\"/Users/me/dev/tabby\" = \"project\"\n")
                .expect("initial policy");
        fs::write(&path, "version = 2\n").expect("invalid replacement");

        assert!(load(&path).is_err());
        assert_eq!(label_for(&active, &pane_with_cwd("tabby"), None), "project");

        fs::write(
            &path,
            "version = 1\n[directories.aliases]\n\"/Users/me/dev/tabby\" = \"repository\"\n",
        )
        .expect("valid replacement");
        let replacement = load(&path).expect("replace active policy");
        assert_eq!(
            label_for(&replacement, &pane_with_cwd("tabby"), None),
            "repository"
        );

        fs::remove_dir_all(directory).expect("remove config directory");
    }

    #[test]
    fn command_aliases_do_not_alias_working_directory_labels() {
        let loaded = parse("version = 1\n[commands.aliases]\ntabby = \"not-a-directory-alias\"\n")
            .expect("valid command alias");

        assert_eq!(candidate(&loaded, "unknown", &["unknown"]), "tabby");
    }

    #[test]
    fn prefixes_keyed_by_classified_candidates_follow_aliases_and_do_not_affect_fallbacks() {
        let loaded = parse(
            r#"
version = 1

[labels]
max_length = 8

[labels.prefixes]
"nvim" = "edit: "
"pnpm lint" = "check: "

[commands.runners]
pnpm = ["lint"]

[commands.aliases]
nvim = "vim"
"pnpm lint" = "style"
"#,
        )
        .expect("valid prefix configuration");

        assert_eq!(candidate(&loaded, "nvim", &["nvim"]), "edit: vi");
        assert_eq!(candidate(&loaded, "pnpm", &["pnpm", "lint"]), "check: s");
        assert_eq!(candidate(&loaded, "bash", &["bash"]), "tabby");
    }

    #[test]
    fn prefix_and_display_width_validation_identifies_the_specific_field() {
        for (contents, field) in [
            (
                "version = 1\n[labels]\nmax_display_width = 0",
                "labels.max_display_width",
            ),
            (
                "version = 1\n[labels.prefixes]\nunknown = \"x\"",
                "labels.prefixes.unknown",
            ),
            (
                "version = 1\n[labels.prefixes]\nnvim = \"\"",
                "labels.prefixes.nvim",
            ),
            (
                "version = 1\n[labels.prefixes]\nnvim = \"bad\\u0000\"",
                "labels.prefixes.nvim",
            ),
            (
                "version = 1\n[commands]\nadditional_ignored = [\"nvim\"]\n[labels.prefixes]\nnvim = \"> \"",
                "labels.prefixes.nvim",
            ),
        ] {
            let error = parse(contents).expect_err("invalid presentation configuration");
            assert!(
                error.to_string().contains(field),
                "diagnostic `{error}` did not identify `{field}`"
            );
        }
    }

    fn label_for(
        loaded: &LoadedConfig,
        pane: &PaneInfo,
        process_info: Option<&PaneProcessInfo>,
    ) -> String {
        loaded
            .policy()
            .candidate_for_pane(pane, process_info)
            .expect("candidate")
            .label()
            .to_string()
    }

    fn candidate(loaded: &LoadedConfig, name: &str, argv: &[&str]) -> String {
        label_for(loaded, &pane_with_cwd("tabby"), Some(&process(name, argv)))
    }

    fn pane_with_cwd(basename: &str) -> PaneInfo {
        PaneInfo {
            pane_id: "workspace:pane".to_string(),
            terminal_id: Some("terminal".to_string()),
            workspace_id: "workspace".to_string(),
            tab_id: "workspace:tab".to_string(),
            focused: true,
            label: None,
            title: None,
            cwd: Some(format!("/Users/me/dev/{basename}")),
            foreground_cwd: None,
            agent: None,
            display_agent: None,
            custom_status: None,
            agent_status: None,
            revision: None,
        }
    }

    fn process(name: &str, argv: &[&str]) -> PaneProcessInfo {
        PaneProcessInfo {
            pane_id: "workspace:pane".to_string(),
            shell_pid: Some(100),
            foreground_process_group_id: Some(200),
            foreground_processes: vec![PaneProcess {
                pid: 201,
                name: name.to_string(),
                argv: Some(argv.iter().map(|value| (*value).to_string()).collect()),
                argv0: argv.first().map(|value| (*value).to_string()),
                cmdline: Some(argv.join(" ")),
                cwd: Some("/Users/me/dev/tabby".to_string()),
            }],
            tty: Some("/dev/ttys001".to_string()),
        }
    }
}
