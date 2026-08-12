use crate::herdr_client::{PaneInfo, PaneProcess, PaneProcessInfo};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const DEFAULT_INTERACTIVE_COMMANDS: &[&str] = &["nvim", "lazygit", "codex", "claude"];
const DEFAULT_RUNNER_SUBCOMMANDS: &[(&str, &str)] = &[
    ("pnpm", "dev"),
    ("npm", "test"),
    ("go", "test"),
    ("cargo", "run"),
];
const DEFAULT_IGNORED_COMMANDS: &[&str] = &[
    "bash", "dash", "env", "fish", "login", "nu", "screen", "sh", "sudo", "tmux", "zsh",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelCandidate {
    label: String,
    source: LabelCandidateSource,
}

impl LabelCandidate {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn source(&self) -> LabelCandidateSource {
        self.source
    }

    pub(crate) fn significant_command(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            source: LabelCandidateSource::SignificantCommand,
        }
    }

    pub(crate) fn working_directory_suffix(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            source: LabelCandidateSource::WorkingDirectorySuffix,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelCandidateSource {
    SignificantCommand,
    WorkingDirectorySuffix,
}

#[derive(Debug, Default)]
pub(crate) struct LabelPresentation {
    pub aliases: BTreeMap<String, String>,
    pub directory_aliases: BTreeMap<String, String>,
    pub prefixes: BTreeMap<String, String>,
    pub max_length: usize,
    pub max_display_width: Option<usize>,
    pub cwd_components: usize,
}

#[derive(Debug, Clone)]
pub struct LabelPolicy {
    significant_commands: BTreeSet<String>,
    runner_subcommands: BTreeSet<(String, String)>,
    builtin_ignored_commands: BTreeSet<String>,
    user_ignored_commands: BTreeSet<String>,
    aliases: BTreeMap<String, String>,
    directory_aliases: BTreeMap<String, String>,
    prefixes: BTreeMap<String, String>,
    max_length: usize,
    max_display_width: Option<usize>,
    cwd_components: usize,
}

impl Default for LabelPolicy {
    fn default() -> Self {
        Self {
            significant_commands: DEFAULT_INTERACTIVE_COMMANDS
                .iter()
                .map(|command| (*command).to_string())
                .collect(),
            runner_subcommands: DEFAULT_RUNNER_SUBCOMMANDS
                .iter()
                .map(|(runner, subcommand)| ((*runner).to_string(), (*subcommand).to_string()))
                .collect(),
            builtin_ignored_commands: DEFAULT_IGNORED_COMMANDS
                .iter()
                .map(|command| (*command).to_string())
                .collect(),
            user_ignored_commands: BTreeSet::new(),
            aliases: BTreeMap::new(),
            directory_aliases: BTreeMap::new(),
            prefixes: BTreeMap::new(),
            max_length: 32,
            max_display_width: None,
            cwd_components: 1,
        }
    }
}

impl LabelPolicy {
    pub(crate) fn is_builtin_significant_command(command: &str) -> bool {
        DEFAULT_INTERACTIVE_COMMANDS.contains(&command)
    }

    pub(crate) fn is_builtin_ignored_command(command: &str) -> bool {
        DEFAULT_IGNORED_COMMANDS.contains(&command)
    }

    pub(crate) fn configured(
        additional_significant: impl IntoIterator<Item = String>,
        additional_ignored: impl IntoIterator<Item = String>,
        runner_subcommands: impl IntoIterator<Item = (String, String)>,
        presentation: LabelPresentation,
    ) -> Self {
        let mut policy = Self::default();
        policy.significant_commands.extend(additional_significant);
        policy.user_ignored_commands.extend(additional_ignored);
        policy.runner_subcommands.extend(runner_subcommands);
        policy.aliases = presentation.aliases;
        policy.directory_aliases = presentation.directory_aliases;
        policy.prefixes = presentation.prefixes;
        policy.max_length = presentation.max_length;
        policy.max_display_width = presentation.max_display_width;
        policy.cwd_components = presentation.cwd_components;
        policy
    }

    pub(crate) fn classified_candidates<'a>(
        additional_significant: impl IntoIterator<Item = &'a str>,
        additional_runners: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> BTreeSet<String> {
        DEFAULT_INTERACTIVE_COMMANDS
            .iter()
            .map(|command| (*command).to_string())
            .chain(
                DEFAULT_RUNNER_SUBCOMMANDS
                    .iter()
                    .map(|(runner, subcommand)| format!("{runner} {subcommand}")),
            )
            .chain(additional_significant.into_iter().map(str::to_string))
            .chain(
                additional_runners
                    .into_iter()
                    .map(|(runner, subcommand)| format!("{runner} {subcommand}")),
            )
            .collect()
    }

    pub fn candidate_for_pane(
        &self,
        pane: &PaneInfo,
        process_info: Option<&PaneProcessInfo>,
    ) -> Option<LabelCandidate> {
        if let Some(process_info) = process_info.filter(|info| info.pane_id == pane.pane_id)
            && let Some(label) = self.significant_command(process_info)
        {
            return Some(LabelCandidate::significant_command(
                self.present_significant_command(&label),
            ));
        }

        self.working_directory_label(pane)
            .map(|label| LabelCandidate::working_directory_suffix(self.truncate_final(&label)))
    }

    fn significant_command(&self, process_info: &PaneProcessInfo) -> Option<String> {
        process_info
            .foreground_processes
            .iter()
            .find_map(|process| self.significant_process_label(process))
    }

    fn significant_process_label(&self, process: &PaneProcess) -> Option<String> {
        let argv = normalized_argv(process);
        let command = argv.first().cloned().or_else(|| basename(&process.name))?;

        if self.user_ignored_commands.contains(&command) {
            return None;
        }

        if self.is_interactive(&command) {
            return Some(command);
        }

        if let Some(subcommand) = argv.get(1).map(String::as_str)
            && self.is_runner_subcommand(&command, subcommand)
        {
            return Some(format!("{command} {subcommand}"));
        }

        if self.builtin_ignored_commands.contains(&command) {
            return None;
        }

        if command == "node"
            && let (Some(runner), Some(subcommand)) = (
                argv.get(1).and_then(|argument| node_runner(argument)),
                argv.get(2).map(String::as_str),
            )
            && !self.user_ignored_commands.contains(runner)
            && self.is_runner_subcommand(runner, subcommand)
        {
            return Some(format!("{runner} {subcommand}"));
        }

        None
    }

    fn is_interactive(&self, command: &str) -> bool {
        self.significant_commands.contains(command)
    }

    fn is_runner_subcommand(&self, command: &str, subcommand: &str) -> bool {
        self.runner_subcommands
            .contains(&(command.to_string(), subcommand.to_string()))
    }

    fn working_directory_label(&self, pane: &PaneInfo) -> Option<String> {
        let cwd = pane.foreground_cwd.as_deref().or(pane.cwd.as_deref())?;
        if let Some(alias) = normalize_absolute_path(Path::new(cwd))
            .and_then(|path| self.directory_aliases.get(&path))
        {
            return Some(alias.clone());
        }
        let components = Path::new(cwd)
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let start = components.len().saturating_sub(self.cwd_components);
        (!components.is_empty()).then(|| components[start..].join("/"))
    }

    fn present_significant_command(&self, classified: &str) -> String {
        let alias = self
            .aliases
            .get(classified)
            .map(String::as_str)
            .unwrap_or(classified);
        let prefix = self
            .prefixes
            .get(classified)
            .map(String::as_str)
            .unwrap_or("");
        self.truncate_final(&format!("{prefix}{alias}"))
    }

    fn truncate_final(&self, label: &str) -> String {
        let scalar_truncated = label.chars().take(self.max_length).collect::<String>();
        let Some(max_display_width) = self.max_display_width else {
            return scalar_truncated;
        };

        let mut display_width = 0;
        let mut result = String::new();
        for grapheme in scalar_truncated.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if display_width + grapheme_width > max_display_width {
                break;
            }
            result.push_str(grapheme);
            display_width += grapheme_width;
        }
        result
    }
}

/// Normalizes an absolute path lexically without accessing the filesystem.
///
/// This intentionally preserves symlink spellings: only `.`, `..`, and
/// redundant separators are collapsed.
pub(crate) fn normalize_absolute_path(path: &Path) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Some(normalized.to_string_lossy().into_owned())
}

fn normalized_argv(process: &PaneProcess) -> Vec<String> {
    process
        .argv
        .as_deref()
        .map(|argv| argv.iter().filter_map(|arg| basename(arg)).collect())
        .or_else(|| {
            process
                .argv0
                .as_deref()
                .map(|argv0| basename(argv0).into_iter().collect())
        })
        .or_else(|| process.cmdline.as_deref().map(split_cmdline))
        .unwrap_or_default()
}

fn split_cmdline(cmdline: &str) -> Vec<String> {
    cmdline.split_whitespace().filter_map(basename).collect()
}

fn node_runner(argument: &str) -> Option<&'static str> {
    match argument {
        "pnpm" | "pnpm.mjs" | "pnpm.cjs" => Some("pnpm"),
        _ => None,
    }
}

fn basename(command: &str) -> Option<String> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }

    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_normalization_clamps_parent_components_at_the_root() {
        assert_eq!(
            normalize_absolute_path(Path::new("/../../code/./tabby")),
            Some("/code/tabby".to_string())
        );
        assert_eq!(
            normalize_absolute_path(Path::new("/code/tabby/../notes")),
            Some("/code/notes".to_string())
        );
    }

    #[test]
    fn labels_interactive_apps_as_significant_commands() {
        for command in ["nvim", "lazygit", "codex", "claude"] {
            let candidate = candidate_for(process(command, &[command]), pane_with_cwd("tabby"));

            assert_eq!(candidate.label(), command);
            assert_eq!(candidate.source(), LabelCandidateSource::SignificantCommand);
        }
    }

    #[test]
    fn labels_runner_subcommand_pairs_as_significant_commands() {
        for (runner, subcommand, expected) in [
            ("pnpm", "dev", "pnpm dev"),
            ("npm", "test", "npm test"),
            ("go", "test", "go test"),
            ("cargo", "run", "cargo run"),
        ] {
            let candidate = candidate_for(
                process(runner, &[runner, subcommand, "--watch"]),
                pane_with_cwd("tabby"),
            );

            assert_eq!(candidate.label(), expected);
            assert_eq!(candidate.source(), LabelCandidateSource::SignificantCommand);
        }
    }

    #[test]
    fn ignores_shells_and_wrappers() {
        for command in ["zsh", "bash", "fish", "sh", "tmux", "screen", "env", "sudo"] {
            let candidate = candidate_for(process(command, &[command]), pane_with_cwd("tabby"));

            assert_eq!(candidate.label(), "tabby");
            assert_eq!(
                candidate.source(),
                LabelCandidateSource::WorkingDirectorySuffix
            );
        }
    }

    #[test]
    fn treats_wrapped_commands_as_opaque_and_falls_back_to_cwd() {
        for argv in [
            &["env", "NODE_ENV=development", "pnpm", "dev"][..],
            &["sudo", "pnpm", "dev"],
            &["zsh", "-lc", "pnpm dev"],
        ] {
            let candidate = candidate_for(process(argv[0], argv), pane_with_cwd("tabby"));

            assert_eq!(candidate.label(), "tabby");
            assert_eq!(
                candidate.source(),
                LabelCandidateSource::WorkingDirectorySuffix
            );
        }
    }

    #[test]
    fn labels_pnpm_dev_when_exposed_through_node_shim() {
        let candidate = candidate_for(
            process(
                "node",
                &[
                    "node",
                    "/Users/me/Library/pnpm/.tools/pnpm/11.1.2/node_modules/pnpm/bin/pnpm.mjs",
                    "dev",
                ],
            ),
            pane_with_cwd("tabby"),
        );

        assert_eq!(candidate.label(), "pnpm dev");
        assert_eq!(candidate.source(), LabelCandidateSource::SignificantCommand);
    }

    #[test]
    fn ignores_transient_and_unknown_processes() {
        for command in ["git", "node", "sleep", "python"] {
            let candidate = candidate_for(
                process(command, &[command, "status"]),
                pane_with_cwd("tabby"),
            );

            assert_eq!(candidate.label(), "tabby");
            assert_eq!(
                candidate.source(),
                LabelCandidateSource::WorkingDirectorySuffix
            );
        }
    }

    #[test]
    fn falls_back_to_working_directory_suffix_without_process_info() {
        let pane = pane_with_cwd("tabby");
        let candidate = LabelPolicy::default()
            .candidate_for_pane(&pane, None)
            .expect("Working Directory Suffix candidate");

        assert_eq!(candidate.label(), "tabby");
        assert_eq!(
            candidate.source(),
            LabelCandidateSource::WorkingDirectorySuffix
        );
    }

    #[test]
    fn prefers_foreground_cwd_for_working_directory_suffix() {
        let mut pane = pane_with_cwd("shell-cwd");
        pane.foreground_cwd = Some("/Users/me/dev/foreground-cwd".to_string());
        let candidate = LabelPolicy::default()
            .candidate_for_pane(&pane, None)
            .expect("foreground Working Directory Suffix candidate");

        assert_eq!(candidate.label(), "foreground-cwd");
        assert_eq!(
            candidate.source(),
            LabelCandidateSource::WorkingDirectorySuffix
        );
    }

    #[test]
    fn falls_back_to_cwd_when_process_info_is_for_a_different_pane() {
        let pane = pane_with_cwd("tabby");
        let mut process_info = process("nvim", &["nvim"]);
        process_info.pane_id = "other:pane".to_string();

        let candidate = LabelPolicy::default()
            .candidate_for_pane(&pane, Some(&process_info))
            .expect("Working Directory Suffix candidate");

        assert_eq!(candidate.label(), "tabby");
        assert_eq!(
            candidate.source(),
            LabelCandidateSource::WorkingDirectorySuffix
        );
    }

    #[test]
    fn normalizes_executable_paths_before_classification() {
        let candidate = candidate_for(
            process("/opt/homebrew/bin/pnpm", &["/opt/homebrew/bin/pnpm", "dev"]),
            pane_with_cwd("tabby"),
        );

        assert_eq!(candidate.label(), "pnpm dev");
        assert_eq!(candidate.source(), LabelCandidateSource::SignificantCommand);
    }

    #[test]
    fn presents_aliases_before_classified_prefixes_then_truncates_once() {
        let mut aliases = BTreeMap::new();
        aliases.insert("nvim".to_string(), "editor".to_string());
        aliases.insert("pnpm dev".to_string(), "serve".to_string());
        let mut prefixes = BTreeMap::new();
        prefixes.insert("nvim".to_string(), ">>".to_string());
        prefixes.insert("pnpm dev".to_string(), "run: ".to_string());
        let policy = LabelPolicy::configured(
            [],
            [],
            [],
            LabelPresentation {
                aliases,
                directory_aliases: BTreeMap::new(),
                prefixes,
                max_length: 5,
                max_display_width: None,
                cwd_components: 1,
            },
        );

        assert_eq!(
            policy
                .candidate_for_pane(&pane_with_cwd("tabby"), Some(&process("nvim", &["nvim"])))
                .expect("candidate")
                .label(),
            ">>edi"
        );
        assert_eq!(
            policy
                .candidate_for_pane(
                    &pane_with_cwd("tabby"),
                    Some(&process("pnpm", &["pnpm", "dev"])),
                )
                .expect("candidate")
                .label(),
            "run: "
        );
        assert_eq!(
            policy
                .candidate_for_pane(&pane_with_cwd("tabby"), Some(&process("bash", &["bash"])))
                .expect("candidate")
                .label(),
            "tabby"
        );
    }

    #[test]
    fn display_width_limit_uses_conservative_grapheme_safe_unicode_width() {
        let label = |policy: &LabelPolicy, command: &str| {
            policy
                .candidate_for_pane(&pane_with_cwd("tabby"), Some(&process(command, &[command])))
                .expect("candidate")
                .label()
                .to_string()
        };

        let mut aliases = BTreeMap::new();
        aliases.insert("ascii".to_string(), "abcd".to_string());
        aliases.insert("cjk".to_string(), "甲乙".to_string());
        aliases.insert("combining".to_string(), "e\u{301}x".to_string());
        aliases.insert("emoji".to_string(), "👩‍💻x".to_string());
        aliases.insert("ambiguous".to_string(), "·x".to_string());
        aliases.insert("private".to_string(), "\u{e000}x".to_string());
        let with_aliases = |max_display_width| {
            LabelPolicy::configured(
                ["ascii", "cjk", "combining", "emoji", "ambiguous", "private"].map(str::to_string),
                [],
                [],
                LabelPresentation {
                    aliases: aliases.clone(),
                    directory_aliases: BTreeMap::new(),
                    prefixes: BTreeMap::new(),
                    max_length: 32,
                    max_display_width: Some(max_display_width),
                    cwd_components: 1,
                },
            )
        };

        assert_eq!(label(&with_aliases(3), "ascii"), "abc");
        assert_eq!(label(&with_aliases(3), "cjk"), "甲");
        assert_eq!(label(&with_aliases(1), "combining"), "e\u{301}");
        assert_eq!(label(&with_aliases(2), "emoji"), "👩‍💻");
        assert_eq!(label(&with_aliases(1), "ambiguous"), "·");
        assert_eq!(label(&with_aliases(1), "private"), "\u{e000}");

        let scalar_policy = LabelPolicy::configured(
            ["combining"].map(str::to_string),
            [],
            [],
            LabelPresentation {
                aliases: BTreeMap::from([("combining".to_string(), "e\u{301}x".to_string())]),
                directory_aliases: BTreeMap::new(),
                prefixes: BTreeMap::new(),
                max_length: 2,
                max_display_width: None,
                cwd_components: 1,
            },
        );
        assert_eq!(label(&scalar_policy, "combining"), "e\u{301}");
    }

    fn candidate_for(process_info: PaneProcessInfo, pane: PaneInfo) -> LabelCandidate {
        LabelPolicy::default()
            .candidate_for_pane(&pane, Some(&process_info))
            .expect("label candidate")
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
                argv: Some(argv.iter().map(|arg| (*arg).to_string()).collect()),
                argv0: argv.first().map(|arg| (*arg).to_string()),
                cmdline: Some(argv.join(" ")),
                cwd: Some("/Users/me/dev/tabby".to_string()),
            }],
            tty: Some("/dev/ttys001".to_string()),
        }
    }
}
