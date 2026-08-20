use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

const HERDR_RPC_TIMEOUT: Duration = Duration::from_millis(75);

pub trait HerdrApi {
    /// Reads a coherent focused tab and pane from `session.snapshot`.
    ///
    /// Foreground process data deliberately remains separate: callers must ask
    /// `pane_process_info` for the selected pane after this observation.
    fn observe_focused_tab(&mut self) -> Result<Option<FocusedTabObservation>, HerdrError>;

    fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo, HerdrError>;
    fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<RenameTabResult, HerdrError>;
}

pub trait RpcTransport {
    fn send_request_line(&mut self, request_line: &str) -> Result<String, HerdrError>;
}

#[derive(Debug)]
pub enum HerdrError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Rpc(RpcError),
    Protocol(String),
}

impl fmt::Display for HerdrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Herdr socket I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "Herdr JSON parsing failed: {error}"),
            Self::Rpc(error) => write!(
                formatter,
                "Herdr RPC error {}: {}",
                error.code, error.message
            ),
            Self::Protocol(message) => write!(formatter, "Herdr protocol error: {message}"),
        }
    }
}

impl std::error::Error for HerdrError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Rpc(_) | Self::Protocol(_) => None,
        }
    }
}

impl From<std::io::Error> for HerdrError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for HerdrError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub struct UnixSocketTransport {
    socket_path: PathBuf,
}

impl UnixSocketTransport {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    pub fn from_env() -> Result<Self, HerdrError> {
        let socket_path = std::env::var_os("HERDR_SOCKET_PATH").ok_or_else(|| {
            HerdrError::Protocol("HERDR_SOCKET_PATH is not set for Herdr socket access".to_string())
        })?;
        Ok(Self::new(socket_path))
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(unix)]
impl RpcTransport for UnixSocketTransport {
    fn send_request_line(&mut self, request_line: &str) -> Result<String, HerdrError> {
        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(HERDR_RPC_TIMEOUT))?;
        stream.set_write_timeout(Some(HERDR_RPC_TIMEOUT))?;
        stream.write_all(request_line.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut response_line = String::new();
        let bytes_read = BufReader::new(stream).read_line(&mut response_line)?;
        if bytes_read == 0 {
            return Err(HerdrError::Protocol(
                "Herdr closed the socket without a response".to_string(),
            ));
        }

        Ok(response_line)
    }
}

#[cfg(not(unix))]
impl RpcTransport for UnixSocketTransport {
    fn send_request_line(&mut self, _request_line: &str) -> Result<String, HerdrError> {
        Err(HerdrError::Protocol(
            "Unix socket transport is unavailable on this platform".to_string(),
        ))
    }
}

pub struct HerdrClient<T> {
    transport: T,
    next_request_id: u64,
}

impl<T> HerdrClient<T>
where
    T: RpcTransport,
{
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_request_id: 1,
        }
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Fetches the Herdr 0.8 session snapshot. Runtime evaluation should prefer
    /// `observe_focused_tab`, which hides focus fallback and coherence rules.
    pub fn session_snapshot(&mut self) -> Result<SessionSnapshot, HerdrError> {
        let result: SessionSnapshotResult = self.call("session.snapshot", EmptyParams {})?;
        result.into_snapshot()
    }

    /// Returns one coherent focused tab/pane observation from `session.snapshot`.
    ///
    /// This intentionally does not fetch foreground processes: `pane.process_info`
    /// is a separate, later observation and cannot be atomically combined with the
    /// snapshot.
    pub fn observe_focused_tab(&mut self) -> Result<Option<FocusedTabObservation>, HerdrError> {
        Ok(self.session_snapshot()?.into_focused_observation())
    }

    fn call<P, R>(&mut self, method: &'static str, params: P) -> Result<R, HerdrError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = self.next_id();
        let request = JsonRpcRequest {
            id: id.clone(),
            method,
            params,
        };
        let request_line = serde_json::to_string(&request)?;
        let response_line = self.transport.send_request_line(&request_line)?;
        decode_response(&id, &response_line)
    }

    fn next_id(&mut self) -> String {
        let id = format!("tabby-{}", self.next_request_id);
        self.next_request_id += 1;
        id
    }
}

impl<T> HerdrApi for HerdrClient<T>
where
    T: RpcTransport,
{
    fn observe_focused_tab(&mut self) -> Result<Option<FocusedTabObservation>, HerdrError> {
        HerdrClient::<T>::observe_focused_tab(self)
    }

    fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo, HerdrError> {
        let result: PaneProcessInfoResult = self.call(
            "pane.process_info",
            PaneProcessInfoParams {
                pane_id: Some(pane_id.to_string()),
            },
        )?;
        result.into_process_info()
    }

    fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<RenameTabResult, HerdrError> {
        self.call(
            "tab.rename",
            TabRenameParams {
                tab_id: tab_id.to_string(),
                label: label.to_string(),
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JsonRpcRequest<P> {
    pub id: String,
    pub method: &'static str,
    pub params: P,
}

impl<P> JsonRpcRequest<P> {
    pub fn new(id: impl Into<String>, method: &'static str, params: P) -> Self {
        Self {
            id: id.into(),
            method,
            params,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct EmptyParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaneProcessInfoParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TabRenameParams {
    pub tab_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct JsonRpcSuccess<R> {
    id: String,
    result: R,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct JsonRpcFailure {
    id: String,
    error: RpcError,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum JsonRpcResponse<R> {
    Success(JsonRpcSuccess<R>),
    Failure(JsonRpcFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneProcessInfoResult {
    #[serde(rename = "type")]
    response_type: String,
    pub process_info: PaneProcessInfo,
}

impl PaneProcessInfoResult {
    fn into_process_info(self) -> Result<PaneProcessInfo, HerdrError> {
        expect_response_type(&self.response_type, "pane_process_info")?;
        Ok(self.process_info)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionSnapshotResult {
    #[serde(rename = "type")]
    response_type: String,
    pub snapshot: SessionSnapshot,
}

impl SessionSnapshotResult {
    pub fn into_snapshot(self) -> Result<SessionSnapshot, HerdrError> {
        expect_response_type(&self.response_type, "session_snapshot")?;
        Ok(self.snapshot)
    }
}

/// The subset of Herdr's `session.snapshot` required for focused observation.
/// Other snapshot sections are deliberately hidden because Tabby does not use
/// them to label a focused tab.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol: u32,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
}

impl SessionSnapshot {
    pub(crate) fn into_focused_observation(self) -> Option<FocusedTabObservation> {
        let selected_tab_id = self
            .focused_tab_id
            .as_deref()
            .filter(|tab_id| self.tabs.iter().any(|tab| tab.tab_id == *tab_id))
            .or_else(|| {
                self.focused_pane_id.as_deref().and_then(|pane_id| {
                    self.panes
                        .iter()
                        .find(|pane| pane.pane_id == pane_id)
                        .map(|pane| pane.tab_id.as_str())
                })
            })
            .or_else(|| {
                self.tabs
                    .iter()
                    .find(|tab| tab.focused)
                    .map(|tab| tab.tab_id.as_str())
            })
            .map(str::to_owned);

        let selected_tab_id = selected_tab_id?;
        let tab = self
            .tabs
            .into_iter()
            .find(|tab| tab.tab_id == selected_tab_id)?;

        let selected_pane_id = self
            .focused_pane_id
            .as_deref()
            .filter(|pane_id| {
                self.panes
                    .iter()
                    .any(|pane| pane.pane_id == *pane_id && pane.tab_id == tab.tab_id)
            })
            .or_else(|| {
                self.panes
                    .iter()
                    .find(|pane| pane.tab_id == tab.tab_id && pane.focused)
                    .map(|pane| pane.pane_id.as_str())
            })
            .or_else(|| {
                self.panes
                    .iter()
                    .find(|pane| pane.tab_id == tab.tab_id)
                    .map(|pane| pane.pane_id.as_str())
            })
            .map(str::to_owned);

        let selected_pane_id = selected_pane_id?;
        let pane = self
            .panes
            .into_iter()
            .find(|pane| pane.pane_id == selected_pane_id && pane.tab_id == tab.tab_id)?;

        Some(FocusedTabObservation {
            working_directory: pane
                .foreground_cwd
                .as_deref()
                .filter(|cwd| !cwd.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    pane.cwd
                        .as_deref()
                        .filter(|cwd| !cwd.is_empty())
                        .map(str::to_owned)
                }),
            tab,
            pane,
        })
    }
}

/// A focused tab/pane pair selected from one Herdr session snapshot.
///
/// `working_directory` prefers `foreground_cwd` and falls back to the pane's
/// cwd. It has no foreground process data; use `pane_process_info` separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedTabObservation {
    pub tab: TabInfo,
    pub pane: PaneInfo,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type")]
pub enum RenameTabResult {
    #[serde(rename = "tab_info")]
    TabInfo { tab: TabInfo },
    #[serde(rename = "ok")]
    Ok,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub number: Option<u64>,
    pub label: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub pane_count: Option<u64>,
    #[serde(default)]
    pub agent_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    #[serde(default)]
    pub terminal_id: Option<String>,
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub custom_status: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneProcessInfo {
    pub pane_id: String,
    #[serde(default)]
    pub shell_pid: Option<u32>,
    #[serde(default)]
    pub foreground_process_group_id: Option<u32>,
    #[serde(default)]
    pub foreground_processes: Vec<PaneProcess>,
    #[serde(default)]
    pub tty: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneProcess {
    pub pid: u32,
    pub name: String,
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub argv0: Option<String>,
    #[serde(default)]
    pub cmdline: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

fn decode_response<R>(expected_id: &str, response_line: &str) -> Result<R, HerdrError>
where
    R: DeserializeOwned,
{
    let response: JsonRpcResponse<R> = serde_json::from_str(response_line)?;
    match response {
        JsonRpcResponse::Success(success) => {
            if success.id != expected_id {
                return Err(HerdrError::Protocol(format!(
                    "expected response id `{expected_id}` but received `{}`",
                    success.id
                )));
            }
            Ok(success.result)
        }
        JsonRpcResponse::Failure(failure) => {
            if failure.id != expected_id {
                return Err(HerdrError::Protocol(format!(
                    "expected error response id `{expected_id}` but received `{}`",
                    failure.id
                )));
            }
            Err(HerdrError::Rpc(failure.error))
        }
    }
}

fn expect_response_type(actual: &str, expected: &str) -> Result<(), HerdrError> {
    if actual == expected {
        Ok(())
    } else {
        Err(HerdrError::Protocol(format!(
            "expected `{expected}` response but received `{actual}`"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;

    const PANE_PROCESS_INFO_FIXTURE: &str =
        include_str!("../tests/fixtures/herdr-client/pane-process-info.json");
    const PANE_PROCESS_INFO_REQUEST_FIXTURE: &str =
        include_str!("../tests/fixtures/herdr-client/pane-process-info-request.json");
    const SESSION_SNAPSHOT_FIXTURE: &str =
        include_str!("../tests/fixtures/herdr-client/session-snapshot.json");
    const SESSION_SNAPSHOT_REQUEST_FIXTURE: &str =
        include_str!("../tests/fixtures/herdr-client/session-snapshot-request.json");
    const SESSION_SNAPSHOT_FALLBACK_FIXTURE: &str =
        include_str!("../tests/fixtures/herdr-client/session-snapshot-fallback.json");
    const TAB_RENAME_REQUEST_FIXTURE: &str =
        include_str!("../tests/fixtures/herdr-client/tab-rename-request.json");
    const TAB_RENAME_RESPONSE_FIXTURE: &str =
        include_str!("../tests/fixtures/herdr-client/tab-rename-response.json");
    const JSONRPC_ERROR_FIXTURE: &str =
        include_str!("../tests/fixtures/herdr-client/jsonrpc-error.json");
    const INVALID_PAYLOAD_FIXTURE: &str =
        include_str!("../tests/fixtures/herdr-client/invalid-payload.json");

    #[test]
    fn deserializes_pane_process_info_fixture() {
        let result: PaneProcessInfoResult =
            decode_response("process-1", PANE_PROCESS_INFO_FIXTURE).expect("process info");
        let info = result
            .into_process_info()
            .expect("pane_process_info response type");

        assert_eq!(info.pane_id, "w1:p1");
        assert_eq!(info.shell_pid, Some(100));
        assert_eq!(info.foreground_processes.len(), 2);
        assert_eq!(info.foreground_processes[0].name, "pnpm");
        assert_eq!(
            info.foreground_processes[0].argv.as_ref().unwrap(),
            &["pnpm", "dev"]
        );
        assert_eq!(
            info.foreground_processes[1].cmdline.as_deref(),
            Some("node server.js")
        );
    }

    #[test]
    fn deserializes_session_snapshot_fixture() {
        let result: SessionSnapshotResult =
            decode_response("snapshot-1", SESSION_SNAPSHOT_FIXTURE).expect("session snapshot");
        let snapshot = result
            .into_snapshot()
            .expect("session_snapshot response type");

        assert_eq!(snapshot.version, "0.8.0");
        assert_eq!(snapshot.protocol, 19);
        assert_eq!(snapshot.focused_tab_id.as_deref(), Some("w1:t1"));
        assert_eq!(snapshot.focused_pane_id.as_deref(), Some("w1:p1"));
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.panes.len(), 2);
    }

    #[test]
    fn observes_focused_tab_from_a_coherent_snapshot_then_processes_separately() {
        let transport = MockTransport::new(vec![
            SESSION_SNAPSHOT_FIXTURE.replace("snapshot-1", "tabby-1"),
            PANE_PROCESS_INFO_FIXTURE.replace("process-1", "tabby-2"),
        ]);
        let mut client = HerdrClient::new(transport);

        let observation = client
            .observe_focused_tab()
            .expect("focused observation")
            .expect("focused tab");
        let process_info = client
            .pane_process_info(&observation.pane.pane_id)
            .expect("separate process observation");

        assert_eq!(observation.tab.tab_id, "w1:t1");
        assert_eq!(observation.pane.pane_id, "w1:p1");
        assert_eq!(
            observation.working_directory.as_deref(),
            Some("/Users/me/dev/tabby")
        );
        assert_eq!(process_info.pane_id, observation.pane.pane_id);

        let transport = client.into_transport();
        let requests: Vec<serde_json::Value> = transport
            .requests
            .iter()
            .map(|request| serde_json::from_str(request).expect("request line json"))
            .collect();
        assert_eq!(requests[0]["method"], "session.snapshot");
        assert_eq!(requests[0]["params"], serde_json::json!({}));
        assert_eq!(requests[1]["method"], "pane.process_info");
    }

    #[test]
    fn focused_observation_falls_back_to_snapshot_flags_and_pane_cwd() {
        let transport = MockTransport::new(vec![
            SESSION_SNAPSHOT_FALLBACK_FIXTURE.replace("snapshot-fallback", "tabby-1"),
        ]);
        let mut client = HerdrClient::new(transport);

        let observation = client
            .observe_focused_tab()
            .expect("focused observation")
            .expect("focused tab");

        assert_eq!(observation.tab.tab_id, "w1:t2");
        assert_eq!(observation.pane.pane_id, "w1:p3");
        assert_eq!(
            observation.working_directory.as_deref(),
            Some("/repo/fallback")
        );
    }

    #[test]
    fn focused_observation_returns_none_when_snapshot_has_no_focused_tab_or_pane() {
        let snapshot = SessionSnapshot {
            version: "0.8.0".to_string(),
            protocol: 19,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            tabs: Vec::new(),
            panes: Vec::new(),
        };

        assert_eq!(snapshot.into_focused_observation(), None);
    }

    #[test]
    fn serializes_pane_process_info_request_fixture() {
        let request = JsonRpcRequest::new(
            "process-1",
            "pane.process_info",
            PaneProcessInfoParams {
                pane_id: Some("w1:p1".to_string()),
            },
        );

        assert_request_matches_fixture(request, PANE_PROCESS_INFO_REQUEST_FIXTURE);
    }

    #[test]
    fn serializes_session_snapshot_request_fixture() {
        let request = JsonRpcRequest::new("snapshot-1", "session.snapshot", EmptyParams {});

        assert_request_matches_fixture(request, SESSION_SNAPSHOT_REQUEST_FIXTURE);
    }

    #[test]
    fn serializes_tab_rename_request_fixture() {
        let request = JsonRpcRequest::new(
            "rename-1",
            "tab.rename",
            TabRenameParams {
                tab_id: "w1:t1".to_string(),
                label: "pnpm dev".to_string(),
            },
        );

        assert_request_matches_fixture(request, TAB_RENAME_REQUEST_FIXTURE);
    }

    #[test]
    fn deserializes_tab_rename_response_fixture() {
        let result: RenameTabResult =
            decode_response("rename-1", TAB_RENAME_RESPONSE_FIXTURE).expect("rename response");

        assert_eq!(
            result,
            RenameTabResult::TabInfo {
                tab: TabInfo {
                    tab_id: "w1:t1".to_string(),
                    workspace_id: "w1".to_string(),
                    number: Some(1),
                    label: "pnpm dev".to_string(),
                    focused: true,
                    pane_count: Some(2),
                    agent_status: Some("idle".to_string()),
                }
            }
        );
    }

    #[test]
    fn jsonrpc_error_becomes_error_variant() {
        let error = decode_response::<SessionSnapshotResult>("tabs-1", JSONRPC_ERROR_FIXTURE)
            .expect_err("rpc error");

        assert!(matches!(
            error,
            HerdrError::Rpc(RpcError { code, message })
                if code == "not_found" && message == "workspace not found"
        ));
    }

    #[test]
    fn invalid_payload_returns_error_without_panic() {
        let error = decode_response::<SessionSnapshotResult>("tabs-1", INVALID_PAYLOAD_FIXTURE)
            .expect_err("invalid payload");

        assert!(matches!(
            error,
            HerdrError::Json(_) | HerdrError::Protocol(_)
        ));
    }

    #[test]
    fn client_methods_serialize_expected_requests() {
        let transport = MockTransport::new(vec![
            SESSION_SNAPSHOT_FIXTURE.replace("snapshot-1", "tabby-1"),
            PANE_PROCESS_INFO_FIXTURE.replace("process-1", "tabby-2"),
            TAB_RENAME_RESPONSE_FIXTURE.replace("rename-1", "tabby-3"),
        ]);
        let mut client = HerdrClient::new(transport);

        client.observe_focused_tab().expect("focused tab");
        client.pane_process_info("w1:p1").expect("process info");
        client.rename_tab("w1:t1", "pnpm dev").expect("rename tab");

        let transport = client.into_transport();
        let requests: Vec<serde_json::Value> = transport
            .requests
            .iter()
            .map(|request| serde_json::from_str(request).expect("request line json"))
            .collect();

        assert_eq!(requests[0]["method"], "session.snapshot");
        assert_eq!(requests[0]["params"], serde_json::json!({}));
        assert_eq!(requests[1]["method"], "pane.process_info");
        assert_eq!(
            requests[1]["params"],
            serde_json::json!({"pane_id":"w1:p1"})
        );
        assert_eq!(requests[2]["method"], "tab.rename");
        assert_eq!(
            requests[2]["params"],
            serde_json::json!({"tab_id":"w1:t1","label":"pnpm dev"})
        );
    }

    #[test]
    fn transport_failure_returns_error_without_panic() {
        let transport =
            MockTransport::with_error(io::Error::new(io::ErrorKind::NotFound, "no socket"));
        let mut client = HerdrClient::new(transport);

        let error = client.observe_focused_tab().expect_err("transport error");
        assert!(matches!(error, HerdrError::Io(_)));
    }

    fn assert_request_matches_fixture<P>(request: JsonRpcRequest<P>, fixture: &str)
    where
        P: Serialize,
    {
        let actual: serde_json::Value = serde_json::to_value(&request).expect("request json");
        let expected: serde_json::Value = serde_json::from_str(fixture).expect("fixture json");

        assert_eq!(actual, expected);
    }

    struct MockTransport {
        responses: VecDeque<String>,
        requests: Vec<String>,
        error: Option<io::Error>,
    }

    impl MockTransport {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses: responses.into(),
                requests: Vec::new(),
                error: None,
            }
        }

        fn with_error(error: io::Error) -> Self {
            Self {
                responses: VecDeque::new(),
                requests: Vec::new(),
                error: Some(error),
            }
        }
    }

    impl RpcTransport for MockTransport {
        fn send_request_line(&mut self, request_line: &str) -> Result<String, HerdrError> {
            self.requests.push(request_line.to_string());
            if let Some(error) = self.error.take() {
                return Err(HerdrError::Io(error));
            }
            self.responses.pop_front().ok_or_else(|| {
                HerdrError::Protocol("mock transport has no queued response".to_string())
            })
        }
    }
}
