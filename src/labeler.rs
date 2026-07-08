use crate::herdr_client::{PaneInfo, PaneProcess, PaneProcessInfo};
use std::path::Path;

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

    pub(crate) fn working_directory_basename(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            source: LabelCandidateSource::WorkingDirectoryBasename,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelCandidateSource {
    SignificantCommand,
    WorkingDirectoryBasename,
}

#[derive(Debug, Clone)]
pub struct LabelPolicy {
    interactive_commands: &'static [&'static str],
    runner_subcommands: &'static [(&'static str, &'static str)],
    ignored_commands: &'static [&'static str],
}

impl Default for LabelPolicy {
    fn default() -> Self {
        Self {
            interactive_commands: DEFAULT_INTERACTIVE_COMMANDS,
            runner_subcommands: DEFAULT_RUNNER_SUBCOMMANDS,
            ignored_commands: DEFAULT_IGNORED_COMMANDS,
        }
    }
}

impl LabelPolicy {
    pub fn candidate_for_pane(
        &self,
        pane: &PaneInfo,
        process_info: Option<&PaneProcessInfo>,
    ) -> Option<LabelCandidate> {
        if let Some(process_info) = process_info.filter(|info| info.pane_id == pane.pane_id)
            && let Some(label) = self.significant_command(process_info)
        {
            return Some(LabelCandidate::significant_command(label));
        }

        working_directory_basename(pane).map(LabelCandidate::working_directory_basename)
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

        if self.is_ignored(&command) {
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

        None
    }

    fn is_interactive(&self, command: &str) -> bool {
        self.interactive_commands.contains(&command)
    }

    fn is_runner_subcommand(&self, command: &str, subcommand: &str) -> bool {
        self.runner_subcommands
            .iter()
            .any(|(runner, expected)| command == *runner && subcommand == *expected)
    }

    fn is_ignored(&self, command: &str) -> bool {
        self.ignored_commands.contains(&command)
    }
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

fn working_directory_basename(pane: &PaneInfo) -> Option<String> {
    pane.foreground_cwd
        .as_deref()
        .or(pane.cwd.as_deref())
        .and_then(basename)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                LabelCandidateSource::WorkingDirectoryBasename
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
                LabelCandidateSource::WorkingDirectoryBasename
            );
        }
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
                LabelCandidateSource::WorkingDirectoryBasename
            );
        }
    }

    #[test]
    fn falls_back_to_working_directory_basename_without_process_info() {
        let pane = pane_with_cwd("tabby");
        let candidate = LabelPolicy::default()
            .candidate_for_pane(&pane, None)
            .expect("cwd basename candidate");

        assert_eq!(candidate.label(), "tabby");
        assert_eq!(
            candidate.source(),
            LabelCandidateSource::WorkingDirectoryBasename
        );
    }

    #[test]
    fn prefers_foreground_cwd_for_working_directory_basename() {
        let mut pane = pane_with_cwd("shell-cwd");
        pane.foreground_cwd = Some("/Users/me/dev/foreground-cwd".to_string());
        let candidate = LabelPolicy::default()
            .candidate_for_pane(&pane, None)
            .expect("foreground cwd basename candidate");

        assert_eq!(candidate.label(), "foreground-cwd");
        assert_eq!(
            candidate.source(),
            LabelCandidateSource::WorkingDirectoryBasename
        );
    }

    #[test]
    fn falls_back_to_cwd_when_process_info_is_for_a_different_pane() {
        let pane = pane_with_cwd("tabby");
        let mut process_info = process("nvim", &["nvim"]);
        process_info.pane_id = "other:pane".to_string();

        let candidate = LabelPolicy::default()
            .candidate_for_pane(&pane, Some(&process_info))
            .expect("cwd basename candidate");

        assert_eq!(candidate.label(), "tabby");
        assert_eq!(
            candidate.source(),
            LabelCandidateSource::WorkingDirectoryBasename
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
