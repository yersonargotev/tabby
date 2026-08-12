//! Versioned user configuration compiled into one validated Label Policy.

use crate::labeler::LabelPolicy;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u8 = 1;
pub const MIN_MAX_LENGTH: usize = 1;
pub const MAX_MAX_LENGTH: usize = 128;
pub const MIN_CWD_COMPONENTS: usize = 1;
pub const MAX_CWD_COMPONENTS: usize = 8;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u8,
    #[serde(default)]
    labels: LabelsConfig,
    #[serde(default)]
    commands: CommandsConfig,
    #[serde(default)]
    directories: DirectoriesConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LabelsConfig {
    max_length: Option<usize>,
    cwd_components: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CommandsConfig {
    additional_significant: Vec<String>,
    additional_ignored: Vec<String>,
    runners: BTreeMap<String, Vec<String>>,
    aliases: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DirectoriesConfig {
    aliases: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct LoadedConfig {
    policy: LabelPolicy,
    source: ConfigSource,
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

    pub(crate) fn built_in_defaults() -> Self {
        Self {
            policy: LabelPolicy::default(),
            source: ConfigSource::BuiltInDefaults,
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
    parse_with_home(contents, std::env::var_os("HOME").as_deref().map(Path::new))
}

fn parse_with_home(contents: &str, home: Option<&Path>) -> Result<LoadedConfig, ConfigError> {
    let config: ConfigFile = toml::from_str(contents).map_err(ConfigError::Toml)?;
    if config.version != SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedVersion(config.version));
    }
    let directory_aliases = validate(&config, home)?;
    let runners = config
        .commands
        .runners
        .into_iter()
        .flat_map(|(runner, subcommands)| {
            subcommands
                .into_iter()
                .map(move |subcommand| (runner.clone(), subcommand))
        });
    Ok(LoadedConfig {
        policy: LabelPolicy::configured(
            config.commands.additional_significant,
            config.commands.additional_ignored,
            runners,
            config.commands.aliases,
            directory_aliases,
            config.labels.max_length.unwrap_or(32),
            config.labels.cwd_components.unwrap_or(1),
        ),
        source: ConfigSource::File,
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

fn validate(
    config: &ConfigFile,
    home: Option<&Path>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    validate_range(
        "labels.max_length",
        config.labels.max_length.unwrap_or(32),
        MIN_MAX_LENGTH,
        MAX_MAX_LENGTH,
    )?;
    validate_range(
        "labels.cwd_components",
        config.labels.cwd_components.unwrap_or(1),
        MIN_CWD_COMPONENTS,
        MAX_CWD_COMPONENTS,
    )?;
    validate_unique_tokens(
        "commands.additional_significant",
        &config.commands.additional_significant,
    )?;
    validate_unique_tokens(
        "commands.additional_ignored",
        &config.commands.additional_ignored,
    )?;
    let ignored = config
        .commands
        .additional_ignored
        .iter()
        .collect::<BTreeSet<_>>();
    if let Some(command) = config
        .commands
        .additional_significant
        .iter()
        .find(|command| ignored.contains(command))
    {
        return validation_error(
            "commands.additional_significant",
            format!("`{command}` is also listed in commands.additional_ignored"),
        );
    }
    if let Some(command) = config
        .commands
        .additional_significant
        .iter()
        .find(|command| LabelPolicy::is_builtin_ignored_command(command))
    {
        return validation_error(
            "commands.additional_significant",
            format!("`{command}` is ignored by the built-in Label Policy"),
        );
    }
    if let Some(command) = config
        .commands
        .additional_significant
        .iter()
        .find(|command| LabelPolicy::is_builtin_significant_command(command))
    {
        return validation_error(
            "commands.additional_significant",
            format!("`{command}` duplicates a built-in Significant Command"),
        );
    }
    for (runner, subcommands) in &config.commands.runners {
        validate_token("commands.runners", runner)?;
        validate_unique_tokens(&format!("commands.runners.{runner}"), subcommands)?;
    }
    for (key, value) in &config.commands.aliases {
        validate_label_text("commands.aliases", key)?;
        validate_label_text(&format!("commands.aliases.{key}"), value)?;
    }
    normalize_directory_aliases(&config.directories.aliases, home)
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
            let error = parse_with_home(contents, home).expect_err("invalid directory alias");
            assert!(
                error.to_string().contains(field),
                "diagnostic `{error}` did not identify `{field}`"
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
        let active = parse("version = 1\n[commands]\nadditional_significant = [\"btop\"]\n")
            .expect("initial policy");
        fs::write(&path, "version = 2\n").expect("invalid replacement");

        assert!(load(&path).is_err());
        assert_eq!(candidate(&active, "btop", &["btop"]), "btop");

        fs::write(
            &path,
            "version = 1\n[commands]\nadditional_significant = [\"yazi\"]\n",
        )
        .expect("valid replacement");
        let replacement = load(&path).expect("replace active policy");
        assert_eq!(candidate(&replacement, "btop", &["btop"]), "tabby");
        assert_eq!(candidate(&replacement, "yazi", &["yazi"]), "yazi");

        fs::remove_dir_all(directory).expect("remove config directory");
    }

    #[test]
    fn command_aliases_do_not_alias_working_directory_labels() {
        let loaded = parse("version = 1\n[commands.aliases]\ntabby = \"not-a-directory-alias\"\n")
            .expect("valid command alias");

        assert_eq!(candidate(&loaded, "unknown", &["unknown"]), "tabby");
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
