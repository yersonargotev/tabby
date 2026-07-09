use crate::paths::{PLUGIN_ID, should_remove_stale_herdr_socket_path};
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const HERDR_BINARY: &str = "herdr";
const HERDR_SOCKET_PATH_ENV: &str = "HERDR_SOCKET_PATH";
const RELEASE_PLUGIN_RELATIVE_PATH: &[&str] = &["share", "tabby"];
const MANIFEST_FILE_NAME: &str = "herdr-plugin.toml";

pub fn relink_from_current_exe() -> Result<String, InstallError> {
    let executable = std::env::current_exe().map_err(InstallError::CurrentExe)?;
    let plugin_root = plugin_root_for_executable(&executable)?;
    let mut runner = SystemCommandRunner;
    relink_with(&plugin_root, &mut runner)
}

pub fn plugin_root_for_executable(executable: &Path) -> Result<PathBuf, InstallError> {
    let bin_dir = executable
        .parent()
        .ok_or_else(|| InstallError::UnexpectedExecutablePath(executable.to_path_buf()))?;
    let install_root = bin_dir
        .parent()
        .ok_or_else(|| InstallError::UnexpectedExecutablePath(executable.to_path_buf()))?;
    let mut plugin_root = install_root.to_path_buf();
    for component in RELEASE_PLUGIN_RELATIVE_PATH {
        plugin_root.push(component);
    }

    let manifest_path = plugin_root.join(MANIFEST_FILE_NAME);
    if !manifest_path.is_file() {
        return Err(InstallError::MissingManifest {
            executable: executable.to_path_buf(),
            manifest_path,
        });
    }

    Ok(plugin_root)
}

pub fn relink_with(
    plugin_root: &Path,
    runner: &mut impl HerdrCommandRunner,
) -> Result<String, InstallError> {
    let plugin_root = plugin_root.to_string_lossy().to_string();

    // Homebrew upgrades can leave Herdr pointing at a cleaned-up versioned Cellar path.
    // Unlink first so the following link always refreshes Herdr to the current install.
    let _ = runner.run(HERDR_BINARY, &["plugin", "unlink", PLUGIN_ID]);

    let link = runner.run(HERDR_BINARY, &["plugin", "link", &plugin_root])?;
    if !link.success {
        return Err(InstallError::HerdrLinkFailed {
            plugin_root,
            stdout: link.stdout,
            stderr: link.stderr,
        });
    }

    Ok(format!(
        "tabby install: linked {PLUGIN_ID} to {plugin_root}\nstart Tabby for the current Herdr Session with: tabby install --start\nor: herdr plugin action invoke start --plugin {PLUGIN_ID}"
    ))
}

pub trait HerdrCommandRunner {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<HerdrCommandOutput, InstallError>;
}

pub struct SystemCommandRunner;

impl HerdrCommandRunner for SystemCommandRunner {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<HerdrCommandOutput, InstallError> {
        let mut command = Command::new(program);
        command.args(args);

        // Herdr panes export HERDR_SOCKET_PATH. If that socket becomes stale after a
        // Herdr restart, plugin link/action commands fail with a vague OS error 2.
        // Let Herdr rediscover the active session from HERDR_SESSION/default config.
        if is_herdr_program(program)
            && should_remove_stale_herdr_socket_path(
                std::env::var_os(HERDR_SOCKET_PATH_ENV).as_deref(),
            )
        {
            command.env_remove(HERDR_SOCKET_PATH_ENV);
        }

        let output = command
            .output()
            .map_err(|source| InstallError::HerdrCommandIo {
                command: command_text(program, args),
                source,
            })?;

        Ok(HerdrCommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrCommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub enum InstallError {
    CurrentExe(io::Error),
    UnexpectedExecutablePath(PathBuf),
    MissingManifest {
        executable: PathBuf,
        manifest_path: PathBuf,
    },
    HerdrCommandIo {
        command: String,
        source: io::Error,
    },
    HerdrLinkFailed {
        plugin_root: String,
        stdout: String,
        stderr: String,
    },
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExe(error) => write!(
                formatter,
                "failed to locate the running tabby executable: {error}"
            ),
            Self::UnexpectedExecutablePath(path) => write!(
                formatter,
                "cannot infer Homebrew install root from executable path `{}`; expected `.../bin/tabby`",
                path.display()
            ),
            Self::MissingManifest {
                executable,
                manifest_path,
            } => write!(
                formatter,
                "cannot find release Herdr manifest `{}` inferred from executable `{}`; run this command from the Homebrew-installed tabby binary, or use `herdr plugin link .` for local development",
                manifest_path.display(),
                executable.display()
            ),
            Self::HerdrCommandIo { command, source } => {
                write!(formatter, "failed to run `{command}`: {source}")
            }
            Self::HerdrLinkFailed {
                plugin_root,
                stdout,
                stderr,
            } => {
                write!(
                    formatter,
                    "failed to link Tabby plugin root `{plugin_root}` with Herdr"
                )?;
                if !stderr.is_empty() {
                    write!(formatter, ": {stderr}")?;
                } else if !stdout.is_empty() {
                    write!(formatter, ": {stdout}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for InstallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentExe(error) => Some(error),
            Self::HerdrCommandIo { source, .. } => Some(source),
            Self::UnexpectedExecutablePath(_)
            | Self::MissingManifest { .. }
            | Self::HerdrLinkFailed { .. } => None,
        }
    }
}

fn command_text(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_herdr_program(program: &str) -> bool {
    Path::new(program).file_name().and_then(OsStr::to_str) == Some(HERDR_BINARY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn infers_release_plugin_root_from_homebrew_binary_path() {
        let temp_dir = TestTempDir::new();
        let executable = temp_dir.path().join("Cellar/tabby/0.1.1/bin/tabby");
        let plugin_root = temp_dir.path().join("Cellar/tabby/0.1.1/share/tabby");
        fs::create_dir_all(executable.parent().expect("bin dir")).expect("create bin dir");
        fs::create_dir_all(&plugin_root).expect("create plugin root");
        fs::write(
            plugin_root.join(MANIFEST_FILE_NAME),
            "id = \"yersonargotev.tabby\"\n",
        )
        .expect("write manifest");

        assert_eq!(
            plugin_root_for_executable(&executable).expect("plugin root"),
            plugin_root
        );
    }

    #[test]
    fn missing_release_manifest_explains_local_development_link() {
        let temp_dir = TestTempDir::new();
        let executable = temp_dir.path().join("target/debug/tabby");
        fs::create_dir_all(executable.parent().expect("debug dir")).expect("create debug dir");

        let error = plugin_root_for_executable(&executable).expect_err("missing manifest");

        assert!(error.to_string().contains("herdr plugin link ."));
    }

    #[test]
    fn relink_ignores_stale_unlink_failure_and_links_current_plugin_root() {
        let mut runner = FakeRunner::new(vec![
            HerdrCommandOutput {
                success: false,
                stdout: "{\"error\":{\"code\":\"plugin_not_found\"}}".to_string(),
                stderr: String::new(),
            },
            HerdrCommandOutput {
                success: true,
                stdout: "linked".to_string(),
                stderr: String::new(),
            },
        ]);
        let plugin_root = Path::new("/opt/homebrew/Cellar/tabby/0.1.1/share/tabby");

        let message = relink_with(plugin_root, &mut runner).expect("relink");

        assert_eq!(
            runner.calls,
            vec![
                vec!["herdr", "plugin", "unlink", PLUGIN_ID],
                vec![
                    "herdr",
                    "plugin",
                    "link",
                    "/opt/homebrew/Cellar/tabby/0.1.1/share/tabby"
                ],
            ]
        );
        assert!(message.contains("tabby install: linked yersonargotev.tabby"));
    }

    #[test]
    fn stale_herdr_socket_path_is_removed_before_running_herdr() {
        let temp_dir = TestTempDir::new();
        let missing_socket = temp_dir.path().join("missing-herdr.sock");

        assert!(should_remove_stale_herdr_socket_path(Some(
            missing_socket.as_os_str()
        )));
    }

    #[test]
    fn existing_herdr_socket_path_is_preserved() {
        let temp_dir = TestTempDir::new();
        let socket_path = temp_dir.path().join("herdr.sock");
        fs::write(&socket_path, "").expect("write socket placeholder");

        assert!(!should_remove_stale_herdr_socket_path(Some(
            socket_path.as_os_str()
        )));
    }

    struct FakeRunner {
        calls: Vec<Vec<String>>,
        outputs: Vec<HerdrCommandOutput>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<HerdrCommandOutput>) -> Self {
            Self {
                calls: Vec::new(),
                outputs,
            }
        }
    }

    impl HerdrCommandRunner for FakeRunner {
        fn run(
            &mut self,
            program: &str,
            args: &[&str],
        ) -> Result<HerdrCommandOutput, InstallError> {
            self.calls.push(
                std::iter::once(program.to_string())
                    .chain(args.iter().map(|arg| (*arg).to_string()))
                    .collect(),
            );
            Ok(self.outputs.remove(0))
        }
    }

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after unix epoch")
                .as_nanos();
            let id = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "tabby-install-test-{}-{unique}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
