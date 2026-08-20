//! Fail-closed validation of the Herdr JSON contract Tabby consumes.

use crate::herdr_client::{
    HerdrApi, HerdrClient, HerdrError, PaneProcessInfo, SessionSnapshot, UnixSocketTransport,
};
use crate::startup::SessionSocket;
use serde_json::Value;
use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus};

const HERDR_BIN_PATH_ENV: &str = "HERDR_BIN_PATH";
const HERDR_SOCKET_PATH_ENV: &str = "HERDR_SOCKET_PATH";
const MINIMUM_HERDR_VERSION: (u64, u64, u64) = (0, 8, 0);
const MINIMUM_HERDR_PROTOCOL: u64 = 19;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedContract {
    version: String,
    protocol: u64,
}

#[derive(Debug)]
pub enum HerdrContractError {
    MissingBinaryPath,
    CommandIo {
        operation: &'static str,
        source: io::Error,
    },
    CommandFailed {
        operation: &'static str,
        status: ExitStatus,
        stderr: String,
    },
    InvalidJson {
        operation: &'static str,
        source: serde_json::Error,
    },
    Incompatible(String),
    LiveProbe {
        method: &'static str,
        source: HerdrError,
    },
}

impl fmt::Display for HerdrContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBinaryPath => write!(
                formatter,
                "{HERDR_BIN_PATH_ENV} is missing; invoke Tabby through the Herdr plugin host so the release-matched binary can be inspected"
            ),
            Self::CommandIo { operation, source } => {
                write!(
                    formatter,
                    "failed to run `{operation}` through {HERDR_BIN_PATH_ENV}: {source}"
                )
            }
            Self::CommandFailed {
                operation,
                status,
                stderr,
            } => write!(formatter, "`{operation}` failed with {status}: {stderr}"),
            Self::InvalidJson { operation, source } => {
                write!(formatter, "`{operation}` returned invalid JSON: {source}")
            }
            Self::Incompatible(message) => formatter.write_str(message),
            Self::LiveProbe { method, source } => {
                write!(formatter, "read-only `{method}` probe failed: {source}")
            }
        }
    }
}

impl std::error::Error for HerdrContractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CommandIo { source, .. } => Some(source),
            Self::InvalidJson { source, .. } => Some(source),
            Self::LiveProbe { source, .. } => Some(source),
            Self::MissingBinaryPath | Self::CommandFailed { .. } | Self::Incompatible(_) => None,
        }
    }
}

trait ContractProbe {
    fn status(&mut self, socket: &SessionSocket) -> Result<Value, HerdrContractError>;
    fn schema(&mut self) -> Result<Value, HerdrContractError>;
    fn session_snapshot(&mut self) -> Result<SessionSnapshot, HerdrError>;
    fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo, HerdrError>;
}

struct SystemContractProbe {
    binary: OsString,
    client: HerdrClient<UnixSocketTransport>,
}

impl SystemContractProbe {
    fn new(binary: OsString, socket: &SessionSocket) -> Self {
        Self {
            binary,
            client: HerdrClient::new(UnixSocketTransport::new(&socket.socket_path)),
        }
    }
}

impl ContractProbe for SystemContractProbe {
    fn status(&mut self, socket: &SessionSocket) -> Result<Value, HerdrContractError> {
        command_json(
            &self.binary,
            "status --json",
            &["status", "--json"],
            Some(&socket.socket_path),
        )
    }

    fn schema(&mut self) -> Result<Value, HerdrContractError> {
        command_json(
            &self.binary,
            "api schema --json",
            &["api", "schema", "--json"],
            None,
        )
    }

    fn session_snapshot(&mut self) -> Result<SessionSnapshot, HerdrError> {
        self.client.session_snapshot()
    }

    fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo, HerdrError> {
        self.client.pane_process_info(pane_id)
    }
}

/// Validates the complete startup contract before the Session Runtime becomes Ready.
pub(crate) fn validate_live(socket: &SessionSocket) -> Result<(), HerdrContractError> {
    let herdr_binary = required_herdr_binary()?;
    let mut probe = SystemContractProbe::new(herdr_binary, socket);
    validate_with(&mut probe, socket)
}

fn validate_with(
    probe: &mut impl ContractProbe,
    socket: &SessionSocket,
) -> Result<(), HerdrContractError> {
    let status = probe.status(socket)?;
    let observed = validate_status(&status, socket)?;

    let schema = probe.schema()?;
    validate_schema(&schema, observed.protocol)?;

    let snapshot = probe
        .session_snapshot()
        .map_err(|source| HerdrContractError::LiveProbe {
            method: "session.snapshot",
            source,
        })?;
    validate_snapshot_contract(&snapshot.version, u64::from(snapshot.protocol), &observed)?;

    if let Some(focused) = snapshot.into_focused_pane_observation() {
        match probe.pane_process_info(&focused.pane.pane_id) {
            Ok(process_info) if process_info.pane_id == focused.pane.pane_id => {}
            Ok(process_info) => {
                return Err(HerdrContractError::Incompatible(format!(
                    "`pane.process_info` probe returned pane `{}` for requested pane `{}`",
                    process_info.pane_id, focused.pane.pane_id
                )));
            }
            Err(_first_error) => {
                // The focused pane can exit between snapshot and process inspection. Retry once
                // from a fresh coherent snapshot before declaring the contract unavailable.
                let retry_snapshot =
                    probe
                        .session_snapshot()
                        .map_err(|source| HerdrContractError::LiveProbe {
                            method: "session.snapshot retry",
                            source,
                        })?;
                validate_snapshot_contract(
                    &retry_snapshot.version,
                    u64::from(retry_snapshot.protocol),
                    &observed,
                )?;
                if let Some(retry_focused) = retry_snapshot.into_focused_pane_observation() {
                    let process_info = probe
                        .pane_process_info(&retry_focused.pane.pane_id)
                        .map_err(|source| HerdrContractError::LiveProbe {
                            method: "pane.process_info",
                            source,
                        })?;
                    if process_info.pane_id != retry_focused.pane.pane_id {
                        return Err(HerdrContractError::Incompatible(format!(
                            "`pane.process_info` probe returned pane `{}` for requested pane `{}`",
                            process_info.pane_id, retry_focused.pane.pane_id
                        )));
                    }
                }
            }
        }
    }

    Ok(())
}

fn required_herdr_binary() -> Result<OsString, HerdrContractError> {
    selected_herdr_binary(env::var_os(HERDR_BIN_PATH_ENV))
}

fn selected_herdr_binary(value: Option<OsString>) -> Result<OsString, HerdrContractError> {
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or(HerdrContractError::MissingBinaryPath)?;
    if !Path::new(&value).is_absolute() {
        return Err(incompatible(format!(
            "{HERDR_BIN_PATH_ENV} must be an absolute path to the host-selected Herdr binary, got `{}`",
            Path::new(&value).display()
        )));
    }
    Ok(value)
}

fn command_json(
    binary: &OsString,
    operation: &'static str,
    args: &[&str],
    socket_path: Option<&Path>,
) -> Result<Value, HerdrContractError> {
    let mut command = Command::new(binary);
    command.args(args);
    if let Some(socket_path) = socket_path {
        command.env(HERDR_SOCKET_PATH_ENV, socket_path);
    }
    let output = command
        .output()
        .map_err(|source| HerdrContractError::CommandIo { operation, source })?;
    if !output.status.success() {
        return Err(HerdrContractError::CommandFailed {
            operation,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    parse_command_json(operation, &output.stdout)
}

fn parse_command_json(operation: &'static str, stdout: &[u8]) -> Result<Value, HerdrContractError> {
    serde_json::from_slice(stdout)
        .map_err(|source| HerdrContractError::InvalidJson { operation, source })
}

fn validate_status(
    status: &Value,
    socket: &SessionSocket,
) -> Result<ObservedContract, HerdrContractError> {
    let server = status
        .get("server")
        .ok_or_else(|| incompatible("status.server is missing"))?;
    let running = server
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let version = server
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let protocol = server.get("protocol").and_then(Value::as_u64);
    let reported_socket = server
        .get("socket")
        .and_then(Value::as_str)
        .unwrap_or_default();

    if !running {
        return Err(incompatible("status.server.running must be true"));
    }
    if !version_at_least_minimum(version) {
        return Err(incompatible(format!(
            "status.server.version `{version}` is older than required Herdr 0.8.0 or is not a numeric release version"
        )));
    }
    let protocol = protocol.ok_or_else(|| incompatible("status.server.protocol is missing"))?;
    if protocol < MINIMUM_HERDR_PROTOCOL {
        return Err(incompatible(format!(
            "status.server.protocol {protocol} is older than required protocol {MINIMUM_HERDR_PROTOCOL}"
        )));
    }
    if reported_socket != socket.socket_path.to_string_lossy() {
        return Err(incompatible(format!(
            "status.server.socket `{reported_socket}` does not match selected socket `{}`",
            socket.socket_path.display()
        )));
    }

    Ok(ObservedContract {
        version: version.to_string(),
        protocol,
    })
}

fn validate_snapshot_contract(
    version: &str,
    protocol: u64,
    observed: &ObservedContract,
) -> Result<(), HerdrContractError> {
    if version != observed.version || protocol != observed.protocol {
        return Err(incompatible(format!(
            "session.snapshot reported {version}/protocol {protocol}, but status reported {}/protocol {}",
            observed.version, observed.protocol
        )));
    }
    Ok(())
}

fn version_at_least_minimum(version: &str) -> bool {
    let mut parts = version.split('.');
    let parsed = (
        parts.next().and_then(|part| part.parse::<u64>().ok()),
        parts.next().and_then(|part| part.parse::<u64>().ok()),
        parts.next().and_then(|part| part.parse::<u64>().ok()),
    );
    parts.next().is_none()
        && matches!(parsed, (Some(major), Some(minor), Some(patch)) if (major, minor, patch) >= MINIMUM_HERDR_VERSION)
}

fn validate_schema(schema: &Value, observed_protocol: u64) -> Result<(), HerdrContractError> {
    let schema_protocol = schema
        .get("protocol")
        .and_then(Value::as_u64)
        .ok_or_else(|| incompatible("api schema.protocol is missing or is not an integer"))?;
    if schema_protocol != observed_protocol {
        return Err(incompatible(format!(
            "api schema protocol {schema_protocol} does not match running protocol {observed_protocol}"
        )));
    }

    let request = schema
        .pointer("/schemas/request")
        .ok_or_else(|| incompatible("api schema.schemas.request is missing"))?;
    validate_request_method(schema, request, "session.snapshot", &[])?;
    validate_request_method(
        schema,
        request,
        "pane.process_info",
        &[("pane_id", "string", false)],
    )?;
    validate_request_method(
        schema,
        request,
        "tab.rename",
        &[("tab_id", "string", true), ("label", "string", true)],
    )?;

    let response = schema
        .pointer("/schemas/success_response")
        .ok_or_else(|| incompatible("api schema.schemas.success_response is missing"))?;
    require_required(schema, response, "result", "success_response")?;
    let response_result = property(schema, response, "result", "success_response")?;
    let response_result = resolve(schema, response_result)?;
    let snapshot = response_payload(schema, response_result, "session_snapshot", "snapshot")?;
    require_field_type(
        schema,
        snapshot,
        "version",
        "string",
        true,
        "session.snapshot",
    )?;
    require_unsigned_integer_field(
        schema,
        snapshot,
        "protocol",
        &["uint32"],
        true,
        "session.snapshot",
    )?;
    for field in ["focused_tab_id", "focused_pane_id"] {
        require_nullable_field_type(schema, snapshot, field, "string", false, "session.snapshot")?;
    }
    let tabs = require_field_type(schema, snapshot, "tabs", "array", true, "session.snapshot")?;
    let tab_info = array_item(schema, tabs, "session.snapshot.tabs")?;
    validate_tab_info(schema, tab_info)?;
    let panes = require_field_type(schema, snapshot, "panes", "array", true, "session.snapshot")?;
    let pane_info = array_item(schema, panes, "session.snapshot.panes")?;
    validate_pane_info(schema, pane_info)?;

    let process_info =
        response_payload(schema, response_result, "pane_process_info", "process_info")?;
    require_field_type(
        schema,
        process_info,
        "pane_id",
        "string",
        true,
        "pane.process_info",
    )?;
    let processes = require_field_type(
        schema,
        process_info,
        "foreground_processes",
        "array",
        false,
        "pane.process_info",
    )?;
    let process = array_item(schema, processes, "pane.process_info.foreground_processes")?;
    require_unsigned_integer_field(
        schema,
        process,
        "pid",
        &["uint32"],
        true,
        "pane.process_info.foreground_processes[]",
    )?;
    require_field_type(
        schema,
        process,
        "name",
        "string",
        true,
        "pane.process_info.foreground_processes[]",
    )?;
    let argv = require_nullable_field_type(
        schema,
        process,
        "argv",
        "array",
        false,
        "pane.process_info.foreground_processes[]",
    )?;
    let argument = array_item(
        schema,
        argv,
        "pane.process_info.foreground_processes[].argv",
    )?;
    if !response_types_are_compatible(schema, argument, "string", false)? {
        return Err(incompatible(
            "pane.process_info.foreground_processes[].argv[] must always be string",
        ));
    }
    for field in ["argv0", "cmdline"] {
        require_nullable_field_type(
            schema,
            process,
            field,
            "string",
            false,
            "pane.process_info.foreground_processes[]",
        )?;
    }

    validate_rename_response(schema, response_result)?;
    Ok(())
}

fn validate_rename_response(
    root: &Value,
    response_schema: &Value,
) -> Result<(), HerdrContractError> {
    if let Some(variant) = find_discriminator(root, response_schema, "type", "tab_info")? {
        let tab_info_compatible = (|| {
            require_required(root, variant, "type", "tab_info")?;
            require_required(root, variant, "tab", "tab_info")?;
            let tab = property(root, variant, "tab", "tab_info")?;
            validate_tab_info(root, resolve(root, tab)?)
        })();
        if tab_info_compatible.is_ok() {
            return Ok(());
        }
    }
    if let Some(variant) = find_discriminator(root, response_schema, "type", "ok")? {
        require_required(root, variant, "type", "ok")?;
        return Ok(());
    }
    Err(incompatible(
        "api schema has no compatible `tab.rename` result shape (`tab_info` or `ok`)",
    ))
}

fn validate_request_method(
    root: &Value,
    request_schema: &Value,
    method: &'static str,
    fields: &[(&str, &str, bool)],
) -> Result<(), HerdrContractError> {
    let variant = find_discriminator(root, request_schema, "method", method)?
        .ok_or_else(|| incompatible(format!("api schema request method `{method}` is missing")))?;
    require_required(root, variant, "method", method)?;
    require_required(root, variant, "params", method)?;
    let params = property(root, variant, "params", method)?;
    let params = resolve(root, params)?;
    if request_accepts_type(root, params, "object")? != Some(true) {
        return Err(incompatible(format!("{method} params must be an object")));
    }
    reject_unsupported_required_fields(
        root,
        params,
        &fields
            .iter()
            .map(|(field, _, _)| *field)
            .collect::<Vec<_>>(),
        method,
        0,
    )?;
    for (field, expected_type, required) in fields {
        require_request_field_type(root, params, field, expected_type, *required, method)?;
    }
    Ok(())
}

fn reject_unsupported_required_fields(
    root: &Value,
    object: &Value,
    supported: &[&str],
    context: &str,
    depth: usize,
) -> Result<(), HerdrContractError> {
    if depth > 16 {
        return Err(incompatible("api schema required-field chain is too deep"));
    }
    let object = resolve(root, object)?;
    if let Some(required) = object.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| incompatible(format!("{context} params.required must be an array")))?;
        for field in required {
            let field = field.as_str().ok_or_else(|| {
                incompatible(format!("{context} params.required entries must be strings"))
            })?;
            if !supported.contains(&field) {
                return Err(incompatible(format!(
                    "{context} params require unsupported field `{field}`"
                )));
            }
        }
    }
    for combinator in ["allOf", "anyOf", "oneOf"] {
        if let Some(variants) = object.get(combinator).and_then(Value::as_array) {
            for variant in variants {
                reject_unsupported_required_fields(root, variant, supported, context, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn response_payload<'a>(
    root: &'a Value,
    response_schema: &'a Value,
    response_type: &str,
    field: &str,
) -> Result<&'a Value, HerdrContractError> {
    let variant =
        find_discriminator(root, response_schema, "type", response_type)?.ok_or_else(|| {
            incompatible(format!(
                "api schema response type `{response_type}` is missing"
            ))
        })?;
    require_required(root, variant, "type", response_type)?;
    require_required(root, variant, field, response_type)?;
    let payload = property(root, variant, field, response_type)?;
    resolve(root, payload)
}

fn validate_tab_info(root: &Value, tab: &Value) -> Result<(), HerdrContractError> {
    for (field, expected_type) in [
        ("tab_id", "string"),
        ("workspace_id", "string"),
        ("label", "string"),
        ("focused", "boolean"),
    ] {
        require_field_type(root, tab, field, expected_type, true, "tab_info")?;
    }
    require_unsigned_integer_field(
        root,
        tab,
        "number",
        &["uint", "uint32", "uint64"],
        true,
        "tab_info",
    )?;
    Ok(())
}

fn validate_pane_info(root: &Value, pane: &Value) -> Result<(), HerdrContractError> {
    for (field, expected_type) in [
        ("pane_id", "string"),
        ("workspace_id", "string"),
        ("tab_id", "string"),
        ("focused", "boolean"),
    ] {
        require_field_type(root, pane, field, expected_type, true, "pane_info")?;
    }
    for field in ["cwd", "foreground_cwd"] {
        require_nullable_field_type(root, pane, field, "string", false, "pane_info")?;
    }
    require_unsigned_integer_field(
        root,
        pane,
        "revision",
        &["uint", "uint32", "uint64"],
        true,
        "pane_info",
    )?;
    Ok(())
}

fn require_field_type<'a>(
    root: &'a Value,
    object: &'a Value,
    field: &str,
    expected_type: &str,
    required: bool,
    context: &str,
) -> Result<&'a Value, HerdrContractError> {
    let values = properties_for_field(root, object, field, 0)?;
    let value = values
        .first()
        .copied()
        .ok_or_else(|| incompatible(format!("{context}.{field} is missing from api schema")))?;
    if required {
        require_required(root, object, field, context)?;
    }
    if !response_properties_are_compatible(root, &values, expected_type, false)? {
        return Err(incompatible(format!(
            "{context}.{field} must always be {expected_type}"
        )));
    }
    resolve(root, value)
}

fn require_nullable_field_type<'a>(
    root: &'a Value,
    object: &'a Value,
    field: &str,
    expected_type: &str,
    required: bool,
    context: &str,
) -> Result<&'a Value, HerdrContractError> {
    let values = properties_for_field(root, object, field, 0)?;
    let value = values
        .first()
        .copied()
        .ok_or_else(|| incompatible(format!("{context}.{field} is missing from api schema")))?;
    if required {
        require_required(root, object, field, context)?;
    }
    if !response_properties_are_compatible(root, &values, expected_type, true)? {
        return Err(incompatible(format!(
            "{context}.{field} must always be {expected_type} or null"
        )));
    }
    resolve(root, value)
}

fn require_unsigned_integer_field<'a>(
    root: &'a Value,
    object: &'a Value,
    field: &str,
    formats: &[&str],
    required: bool,
    context: &str,
) -> Result<&'a Value, HerdrContractError> {
    let value = require_field_type(root, object, field, "integer", required, context)?;
    let properties = properties_for_field(root, object, field, 0)?;
    for property in properties {
        if !integer_schema_uses_formats(root, property, formats)? {
            return Err(incompatible(format!(
                "{context}.{field} must use one of the integer formats {formats:?}"
            )));
        }
    }
    Ok(value)
}

fn require_request_field_type<'a>(
    root: &'a Value,
    object: &'a Value,
    field: &str,
    expected_type: &str,
    required: bool,
    context: &str,
) -> Result<&'a Value, HerdrContractError> {
    let values = properties_for_field(root, object, field, 0)?;
    let value = values
        .first()
        .copied()
        .ok_or_else(|| incompatible(format!("{context}.{field} is missing from api schema")))?;
    if required {
        require_required(root, object, field, context)?;
    }
    for value in &values {
        if request_accepts_type(root, value, expected_type)? != Some(true) {
            return Err(incompatible(format!(
                "{context}.{field} does not accept {expected_type}"
            )));
        }
    }
    resolve(root, value)
}

fn property<'a>(
    root: &'a Value,
    object: &'a Value,
    field: &str,
    context: &str,
) -> Result<&'a Value, HerdrContractError> {
    property_if_present(root, object, field, 0)?
        .ok_or_else(|| incompatible(format!("{context}.{field} is missing from api schema")))
}

fn property_if_present<'a>(
    root: &'a Value,
    object: &'a Value,
    field: &str,
    depth: usize,
) -> Result<Option<&'a Value>, HerdrContractError> {
    Ok(properties_for_field(root, object, field, depth)?
        .first()
        .copied())
}

fn properties_for_field<'a>(
    root: &'a Value,
    object: &'a Value,
    field: &str,
    depth: usize,
) -> Result<Vec<&'a Value>, HerdrContractError> {
    if depth > 16 {
        return Err(incompatible("api schema allOf chain is too deep"));
    }
    let object = resolve(root, object)?;
    let mut found = Vec::new();
    if let Some(value) = object.pointer(&format!("/properties/{field}")) {
        found.push(value);
    }
    if let Some(variants) = object.get("allOf").and_then(Value::as_array) {
        for variant in variants {
            found.extend(properties_for_field(root, variant, field, depth + 1)?);
        }
    }
    Ok(found)
}

fn array_item<'a>(
    root: &'a Value,
    array: &'a Value,
    context: &str,
) -> Result<&'a Value, HerdrContractError> {
    let items = array
        .get("items")
        .ok_or_else(|| incompatible(format!("{context}.items is missing from api schema")))?;
    resolve(root, items)
}

fn require_required(
    root: &Value,
    object: &Value,
    field: &str,
    context: &str,
) -> Result<(), HerdrContractError> {
    if field_is_required(root, object, field, 0)? {
        Ok(())
    } else {
        Err(incompatible(format!(
            "{context}.{field} is not required by api schema"
        )))
    }
}

fn field_is_required(
    root: &Value,
    object: &Value,
    field: &str,
    depth: usize,
) -> Result<bool, HerdrContractError> {
    if depth > 16 {
        return Err(incompatible("api schema allOf chain is too deep"));
    }
    let object = resolve(root, object)?;
    if object
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|fields| fields.iter().any(|value| value.as_str() == Some(field)))
    {
        return Ok(true);
    }
    let Some(variants) = object.get("allOf").and_then(Value::as_array) else {
        return Ok(false);
    };
    for variant in variants {
        if field_is_required(root, variant, field, depth + 1)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn request_accepts_type(
    root: &Value,
    value: &Value,
    expected: &str,
) -> Result<Option<bool>, HerdrContractError> {
    let value = resolve(root, value)?;
    if !has_only_supported_type_keywords(value) {
        return Ok(None);
    }
    match value.get("type") {
        Some(Value::String(actual)) => Ok(Some(actual == expected)),
        Some(Value::Array(types)) => Ok(Some(
            types.iter().any(|value| value.as_str() == Some(expected)),
        )),
        _ => {
            if let Some(variants) = value.get("anyOf").and_then(Value::as_array) {
                let mut unknown = false;
                for variant in variants {
                    match request_accepts_type(root, variant, expected)? {
                        Some(true) => return Ok(Some(true)),
                        Some(false) => {}
                        None => unknown = true,
                    }
                }
                return Ok((!unknown).then_some(false));
            }
            if let Some(variants) = value.get("oneOf").and_then(Value::as_array) {
                let mut matches = 0;
                for variant in variants {
                    match request_accepts_type(root, variant, expected)? {
                        Some(true) => matches += 1,
                        Some(false) => {}
                        None => return Ok(None),
                    }
                }
                return Ok(Some(matches == 1));
            }
            if let Some(variants) = value.get("allOf").and_then(Value::as_array) {
                let mut unknown = false;
                for variant in variants {
                    match request_accepts_type(root, variant, expected)? {
                        Some(false) => return Ok(Some(false)),
                        Some(true) => {}
                        None => unknown = true,
                    }
                }
                return Ok((!unknown).then_some(true));
            }
            Ok(Some(true))
        }
    }
}

fn response_types_are_compatible(
    root: &Value,
    value: &Value,
    expected: &str,
    nullable: bool,
) -> Result<bool, HerdrContractError> {
    let Some(types) = response_type_set(root, value, 0)? else {
        return Ok(false);
    };
    Ok(types.contains(expected)
        && types
            .iter()
            .all(|actual| actual == expected || nullable && actual == "null"))
}

fn response_properties_are_compatible(
    root: &Value,
    values: &[&Value],
    expected: &str,
    nullable: bool,
) -> Result<bool, HerdrContractError> {
    let mut intersection: Option<BTreeSet<String>> = None;
    for value in values {
        let Some(types) = response_type_set(root, value, 0)? else {
            return Ok(false);
        };
        intersection = Some(match intersection {
            Some(current) => current.intersection(&types).cloned().collect(),
            None => types,
        });
    }
    let Some(types) = intersection else {
        return Ok(false);
    };
    Ok(types.contains(expected)
        && types
            .iter()
            .all(|actual| actual == expected || nullable && actual == "null"))
}

fn response_type_set(
    root: &Value,
    value: &Value,
    depth: usize,
) -> Result<Option<BTreeSet<String>>, HerdrContractError> {
    if depth > 16 {
        return Err(incompatible("api schema type combinator chain is too deep"));
    }
    let value = resolve(root, value)?;
    if !has_only_supported_type_keywords(value) {
        return Ok(None);
    }
    if let Some(declared) = value.get("type") {
        let types = match declared {
            Value::String(actual) => [actual.clone()].into_iter().collect(),
            Value::Array(actual) => actual
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => BTreeSet::new(),
        };
        return Ok(Some(types));
    }
    if let Some(variants) = value.get("anyOf").and_then(Value::as_array) {
        let mut union = BTreeSet::new();
        for variant in variants {
            let Some(types) = response_type_set(root, variant, depth + 1)? else {
                return Ok(None);
            };
            union.extend(types);
        }
        return Ok(Some(union));
    }
    if let Some(variants) = value.get("oneOf").and_then(Value::as_array) {
        let mut branch_types = Vec::new();
        for variant in variants {
            let Some(types) = response_type_set(root, variant, depth + 1)? else {
                return Ok(None);
            };
            if branch_types
                .iter()
                .any(|existing: &BTreeSet<String>| !existing.is_disjoint(&types))
            {
                return Ok(None);
            }
            branch_types.push(types);
        }
        return Ok(Some(branch_types.into_iter().flatten().collect()));
    }
    if let Some(variants) = value.get("allOf").and_then(Value::as_array) {
        let mut intersection: Option<BTreeSet<String>> = None;
        for variant in variants {
            let Some(types) = response_type_set(root, variant, depth + 1)? else {
                return Ok(None);
            };
            intersection = Some(match intersection {
                Some(current) => current.intersection(&types).cloned().collect(),
                None => types,
            });
        }
        return Ok(intersection);
    }
    Ok(None)
}

fn integer_schema_uses_formats(
    root: &Value,
    value: &Value,
    formats: &[&str],
) -> Result<bool, HerdrContractError> {
    let value = resolve(root, value)?;
    if let Some(declared) = value.get("type") {
        let contains_integer = match declared {
            Value::String(actual) => actual == "integer",
            Value::Array(actual) => actual.iter().any(|value| value.as_str() == Some("integer")),
            _ => return Ok(false),
        };
        if !contains_integer {
            return Ok(true);
        }
        return Ok(value
            .get("format")
            .and_then(Value::as_str)
            .is_some_and(|format| formats.contains(&format)));
    }
    for combinator in ["anyOf", "oneOf", "allOf"] {
        if let Some(variants) = value.get(combinator).and_then(Value::as_array) {
            for variant in variants {
                if !integer_schema_uses_formats(root, variant, formats)? {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_only_supported_type_keywords(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.keys().all(|key| {
        matches!(
            key.as_str(),
            "$ref"
                | "type"
                | "anyOf"
                | "oneOf"
                | "allOf"
                | "properties"
                | "required"
                | "items"
                | "additionalProperties"
                | "description"
                | "title"
                | "default"
                | "examples"
                | "deprecated"
                | "readOnly"
                | "writeOnly"
                | "$comment"
                | "format"
                | "minimum"
                | "maximum"
                | "exclusiveMinimum"
                | "exclusiveMaximum"
        )
    })
}

fn resolve<'a>(root: &'a Value, value: &'a Value) -> Result<&'a Value, HerdrContractError> {
    let mut current = value;
    for _ in 0..16 {
        let Some(reference) = current.get("$ref").and_then(Value::as_str) else {
            return Ok(current);
        };
        if let Some(object) = current.as_object()
            && object
                .keys()
                .any(|key| key != "$ref" && !is_annotation_keyword(key))
        {
            return Err(incompatible(format!(
                "api schema reference `{reference}` has unsupported assertion siblings"
            )));
        }
        let pointer = reference.strip_prefix('#').ok_or_else(|| {
            incompatible(format!(
                "external api schema reference `{reference}` is unsupported"
            ))
        })?;
        current = root.pointer(pointer).ok_or_else(|| {
            incompatible(format!(
                "api schema reference `{reference}` cannot be resolved"
            ))
        })?;
    }
    Err(incompatible("api schema reference chain is too deep"))
}

fn is_annotation_keyword(keyword: &str) -> bool {
    matches!(
        keyword,
        "description"
            | "title"
            | "default"
            | "examples"
            | "deprecated"
            | "readOnly"
            | "writeOnly"
            | "$comment"
    )
}

fn find_discriminator<'a>(
    root: &'a Value,
    value: &'a Value,
    property: &str,
    expected: &str,
) -> Result<Option<&'a Value>, HerdrContractError> {
    find_discriminator_with_depth(root, value, property, expected, 0)
}

fn find_discriminator_with_depth<'a>(
    root: &'a Value,
    value: &'a Value,
    property: &str,
    expected: &str,
    depth: usize,
) -> Result<Option<&'a Value>, HerdrContractError> {
    if depth > 16 {
        return Err(incompatible("api schema combinator chain is too deep"));
    }
    let value = resolve(root, value)?;
    let discriminators = properties_for_field(root, value, property, depth)?;
    let mut saw_expected = false;
    let mut contradiction = false;
    for discriminator in discriminators {
        let discriminator = resolve(root, discriminator)?;
        if let Some(actual) = discriminator.get("const").and_then(Value::as_str) {
            saw_expected |= actual == expected;
            contradiction |= actual != expected;
        }
    }
    if saw_expected && !contradiction {
        return Ok(Some(value));
    }

    for combinator in ["oneOf", "anyOf", "allOf"] {
        let Some(variants) = value.get(combinator).and_then(Value::as_array) else {
            continue;
        };
        for variant in variants {
            if let Some(found) =
                find_discriminator_with_depth(root, variant, property, expected, depth + 1)?
            {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

fn incompatible(message: impl Into<String>) -> HerdrContractError {
    HerdrContractError::Incompatible(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr_client::{PaneInfo, TabInfo};
    use serde_json::json;
    use std::collections::VecDeque;

    struct FakeProbe {
        status: Value,
        schema: Value,
        snapshots: VecDeque<Result<SessionSnapshot, HerdrError>>,
        process_results: VecDeque<Result<PaneProcessInfo, HerdrError>>,
        requested_panes: Vec<String>,
    }

    impl ContractProbe for FakeProbe {
        fn status(&mut self, _socket: &SessionSocket) -> Result<Value, HerdrContractError> {
            Ok(self.status.clone())
        }

        fn schema(&mut self) -> Result<Value, HerdrContractError> {
            Ok(self.schema.clone())
        }

        fn session_snapshot(&mut self) -> Result<SessionSnapshot, HerdrError> {
            self.snapshots
                .pop_front()
                .expect("unexpected session.snapshot probe")
        }

        fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo, HerdrError> {
            self.requested_panes.push(pane_id.to_string());
            self.process_results
                .pop_front()
                .expect("unexpected pane.process_info probe")
        }
    }

    fn status(version: &str, protocol: u64, socket: &str) -> Value {
        json!({"server": {"running": true, "version": version, "protocol": protocol, "socket": socket}})
    }

    fn compatible_schema(protocol: u64) -> Value {
        json!({
            "protocol": protocol,
            "schemas": {
                "request": {
                    "oneOf": [
                        {"type":"object","properties":{"method":{"const":"session.snapshot"},"params":{"$ref":"#/schemas/request/$defs/EmptyParams"}},"required":["method","params"]},
                        {"type":"object","properties":{"method":{"const":"tab.rename"},"params":{"$ref":"#/schemas/request/$defs/TabRenameParams"}},"required":["method","params"]},
                        {"type":"object","properties":{"method":{"const":"pane.process_info"},"params":{"$ref":"#/schemas/request/$defs/PaneProcessInfoParams"}},"required":["method","params"]}
                    ],
                    "$defs": {
                        "EmptyParams":{"type":"object"},
                        "StringValue":{"type":"string"},
                        "PaneProcessInfoParams":{"type":"object","properties":{"pane_id":{"type":["string","null"]}}},
                        "TabRenameParams":{"type":"object","properties":{"tab_id":{"type":"string"},"label":{"type":"string"}},"required":["tab_id","label"]}
                    }
                },
                "success_response": {
                    "type": "object",
                    "properties": {
                        "result": {"$ref":"#/schemas/success_response/$defs/ResponseResult"}
                    },
                    "required": ["result"],
                    "$defs": {
                        "ResponseResult":{"oneOf":[
                            {"properties":{"type":{"const":"session_snapshot"},"snapshot":{"$ref":"#/schemas/success_response/$defs/SessionSnapshot"}},"required":["type","snapshot"]},
                            {"properties":{"type":{"const":"tab_info"},"tab":{"$ref":"#/schemas/success_response/$defs/TabInfo"}},"required":["type","tab"]},
                            {"properties":{"type":{"const":"pane_process_info"},"process_info":{"$ref":"#/schemas/success_response/$defs/PaneProcessInfo"}},"required":["type","process_info"]},
                            {"properties":{"type":{"const":"ok"}},"required":["type"]}
                        ]},
                        "SessionSnapshot":{"type":"object","properties":{"version":{"type":"string"},"protocol":{"type":"integer","format":"uint32","minimum":0},"focused_tab_id":{"type":["string","null"]},"focused_pane_id":{"type":["string","null"]},"tabs":{"type":"array","items":{"$ref":"#/schemas/success_response/$defs/TabInfo"}},"panes":{"type":"array","items":{"$ref":"#/schemas/success_response/$defs/PaneInfo"}}},"required":["version","protocol","tabs","panes"]},
                        "TabInfo":{"type":"object","properties":{"tab_id":{"type":"string"},"workspace_id":{"type":"string"},"number":{"type":"integer","format":"uint64","minimum":0},"label":{"type":"string"},"focused":{"type":"boolean"}},"required":["tab_id","workspace_id","number","label","focused"]},
                        "PaneInfo":{"type":"object","properties":{"pane_id":{"type":"string"},"workspace_id":{"type":"string"},"tab_id":{"type":"string"},"focused":{"type":"boolean"},"cwd":{"type":["string","null"]},"foreground_cwd":{"type":["null","string"]},"revision":{"type":"integer","format":"uint64","minimum":0}},"required":["pane_id","workspace_id","tab_id","focused","revision"]},
                        "PaneProcessInfo":{"type":"object","properties":{"pane_id":{"type":"string"},"foreground_processes":{"type":"array","items":{"$ref":"#/schemas/success_response/$defs/PaneProcessInfoProcess"}}},"required":["pane_id"]},
                        "PaneProcessInfoProcess":{"type":"object","properties":{"pid":{"type":"integer","format":"uint32","minimum":0},"name":{"type":"string"},"argv":{"type":["array","null"],"items":{"type":"string"}},"argv0":{"type":["string","null"]},"cmdline":{"anyOf":[{"type":"string"},{"type":"null"}]}},"required":["pid","name"]}
                    }
                }
            },
            "future_addition": {"accepted": true}
        })
    }

    fn snapshot(version: &str, protocol: u32, focused: bool) -> SessionSnapshot {
        SessionSnapshot {
            version: version.to_string(),
            protocol,
            focused_workspace_id: focused.then(|| "workspace-1".to_string()),
            focused_tab_id: focused.then(|| "tab-1".to_string()),
            focused_pane_id: focused.then(|| "pane-1".to_string()),
            tabs: focused
                .then(|| TabInfo {
                    tab_id: "tab-1".to_string(),
                    workspace_id: "workspace-1".to_string(),
                    number: Some(1),
                    label: "shell".to_string(),
                    focused: true,
                    pane_count: Some(1),
                    agent_status: None,
                })
                .into_iter()
                .collect(),
            panes: focused
                .then(|| PaneInfo {
                    pane_id: "pane-1".to_string(),
                    terminal_id: Some("terminal-1".to_string()),
                    workspace_id: "workspace-1".to_string(),
                    tab_id: "tab-1".to_string(),
                    focused: true,
                    label: None,
                    title: None,
                    cwd: Some("/tmp".to_string()),
                    foreground_cwd: None,
                    agent: None,
                    display_agent: None,
                    custom_status: None,
                    agent_status: None,
                    revision: Some(1),
                })
                .into_iter()
                .collect(),
        }
    }

    fn fake_probe(version: &str, protocol: u64, focused: bool) -> FakeProbe {
        FakeProbe {
            status: status(version, protocol, "/tmp/herdr-contract.sock"),
            schema: compatible_schema(protocol),
            snapshots: VecDeque::from([Ok(snapshot(version, protocol as u32, focused))]),
            process_results: if focused {
                VecDeque::from([Ok(PaneProcessInfo {
                    pane_id: "pane-1".to_string(),
                    shell_pid: None,
                    foreground_process_group_id: None,
                    foreground_processes: Vec::new(),
                    tty: None,
                })])
            } else {
                VecDeque::new()
            },
            requested_panes: Vec::new(),
        }
    }

    #[test]
    fn accepts_verified_and_compatible_later_contracts() {
        let socket = SessionSocket::resolve("/tmp/herdr-contract.sock").expect("socket");
        for (version, protocol) in [("0.8.0", 19), ("0.8.2", 20), ("0.8.3", 21)] {
            let observed = validate_status(
                &status(version, protocol, "/tmp/herdr-contract.sock"),
                &socket,
            )
            .expect("status");
            assert_eq!(observed.version, version);
            validate_schema(&compatible_schema(protocol), protocol).expect("schema");
            validate_snapshot_contract(version, protocol, &observed).expect("snapshot");
        }
    }

    #[test]
    fn validates_read_only_live_probes_for_a_compatible_later_protocol() {
        let socket = SessionSocket::resolve("/tmp/herdr-contract.sock").expect("socket");
        let mut probe = fake_probe("0.8.3", 21, true);

        validate_with(&mut probe, &socket).expect("compatible live contract");

        assert_eq!(probe.requested_panes, ["pane-1"]);
        assert!(probe.snapshots.is_empty());
        assert!(probe.process_results.is_empty());
    }

    #[test]
    fn accepts_a_session_without_a_focused_pane_without_a_process_probe() {
        let socket = SessionSocket::resolve("/tmp/herdr-contract.sock").expect("socket");
        let mut probe = fake_probe("0.8.2", 20, false);

        validate_with(&mut probe, &socket).expect("empty session contract");

        assert!(probe.requested_panes.is_empty());
    }

    #[test]
    fn does_not_probe_an_unfocused_first_pane_in_the_focused_tab() {
        let socket = SessionSocket::resolve("/tmp/herdr-contract.sock").expect("socket");
        let mut probe = fake_probe("0.8.2", 20, true);
        let snapshot = probe
            .snapshots
            .front_mut()
            .expect("snapshot")
            .as_mut()
            .expect("valid snapshot");
        snapshot.focused_pane_id = None;
        snapshot.panes[0].focused = false;

        validate_with(&mut probe, &socket).expect("compatible contract without a focused pane");

        assert!(probe.requested_panes.is_empty());
    }

    #[test]
    fn retries_a_process_probe_once_when_the_focused_pane_disappears() {
        let socket = SessionSocket::resolve("/tmp/herdr-contract.sock").expect("socket");
        let mut probe = fake_probe("0.8.2", 20, true);
        probe.process_results =
            VecDeque::from([Err(HerdrError::Protocol("pane disappeared".to_string()))]);
        probe.snapshots.push_back(Ok(snapshot("0.8.2", 20, false)));

        validate_with(&mut probe, &socket).expect("transient process race");

        assert_eq!(probe.requested_panes, ["pane-1"]);
    }

    #[test]
    fn reports_snapshot_and_process_probe_failures_by_method() {
        let socket = SessionSocket::resolve("/tmp/herdr-contract.sock").expect("socket");
        let mut snapshot_failure = fake_probe("0.8.2", 20, false);
        snapshot_failure.snapshots = VecDeque::from([Err(HerdrError::Protocol(
            "snapshot unavailable".to_string(),
        ))]);
        let error = validate_with(&mut snapshot_failure, &socket).expect_err("snapshot failure");
        assert!(error.to_string().contains("session.snapshot"));

        let mut process_failure = fake_probe("0.8.2", 20, true);
        process_failure.process_results = VecDeque::from([
            Err(HerdrError::Protocol("pane disappeared".to_string())),
            Err(HerdrError::Protocol(
                "process probe unavailable".to_string(),
            )),
        ]);
        process_failure
            .snapshots
            .push_back(Ok(snapshot("0.8.2", 20, true)));
        let error = validate_with(&mut process_failure, &socket).expect_err("process failure");
        assert!(error.to_string().contains("pane.process_info"));
    }

    #[test]
    fn requires_the_host_selected_herdr_binary_without_a_path_fallback() {
        assert!(matches!(
            selected_herdr_binary(None),
            Err(HerdrContractError::MissingBinaryPath)
        ));
        assert!(matches!(
            selected_herdr_binary(Some(OsString::new())),
            Err(HerdrContractError::MissingBinaryPath)
        ));
        let relative = selected_herdr_binary(Some(OsString::from("herdr")))
            .expect_err("relative path must not search PATH");
        assert!(relative.to_string().contains("absolute path"));
        assert_eq!(
            selected_herdr_binary(Some(OsString::from("/selected/herdr")))
                .expect("selected binary"),
            OsString::from("/selected/herdr")
        );
    }

    #[test]
    fn rejects_old_or_contradictory_status() {
        let socket = SessionSocket::resolve("/tmp/herdr-contract.sock").expect("socket");
        for candidate in [
            status("0.7.5", 19, "/tmp/herdr-contract.sock"),
            status("0.8.0-preview", 19, "/tmp/herdr-contract.sock"),
            status("0.8.0", 18, "/tmp/herdr-contract.sock"),
            status("0.8.2", 20, "/tmp/other.sock"),
        ] {
            assert!(validate_status(&candidate, &socket).is_err());
        }
    }

    #[test]
    fn rejects_missing_method_and_incompatible_required_field() {
        let mut missing_method = compatible_schema(20);
        missing_method["schemas"]["request"]["oneOf"]
            .as_array_mut()
            .expect("oneOf")
            .retain(|variant| variant["properties"]["method"]["const"] != "tab.rename");
        let error = validate_schema(&missing_method, 20).expect_err("missing method");
        assert!(error.to_string().contains("tab.rename"));

        let mut wrong_field = compatible_schema(20);
        wrong_field["schemas"]["success_response"]["$defs"]["PaneInfo"]["properties"]["tab_id"]["type"] =
            json!("integer");
        let error = validate_schema(&wrong_field, 20).expect_err("wrong field");
        assert!(error.to_string().contains("pane_info.tab_id"));
    }

    #[test]
    fn rejects_request_parameters_that_tabby_cannot_supply() {
        let mut snapshot_extra = compatible_schema(20);
        snapshot_extra["schemas"]["request"]["$defs"]["EmptyParams"] = json!({
            "type": "object",
            "properties": {"future": {"type": "string"}},
            "required": ["future"]
        });
        let error = validate_schema(&snapshot_extra, 20).expect_err("unsupported snapshot param");
        assert!(error.to_string().contains("session.snapshot"));
        assert!(error.to_string().contains("future"));

        let mut rename_extra = compatible_schema(20);
        rename_extra["schemas"]["request"]["$defs"]["TabRenameParams"] = json!({
            "allOf": [
                {"type": "object", "properties": {"tab_id": {"type": "string"}, "label": {"type": "string"}}, "required": ["tab_id", "label"]},
                {"properties": {"workspace_id": {"type": "string"}}, "required": ["workspace_id"]}
            ]
        });
        let error = validate_schema(&rename_extra, 20).expect_err("unsupported rename param");
        assert!(error.to_string().contains("tab.rename"));
        assert!(error.to_string().contains("workspace_id"));
    }

    #[test]
    fn rejects_incompatible_snapshot_focus_selectors() {
        for field in ["focused_tab_id", "focused_pane_id"] {
            let mut schema = compatible_schema(20);
            schema["schemas"]["success_response"]["$defs"]["SessionSnapshot"]["properties"]
                [field]["type"] = json!("integer");

            let error = validate_schema(&schema, 20).expect_err("incompatible focus selector");
            assert!(error.to_string().contains(field));
        }
    }

    #[test]
    fn rejects_ambiguous_response_types_and_missing_process_fields() {
        let mut ambiguous_label = compatible_schema(20);
        ambiguous_label["schemas"]["success_response"]["$defs"]["TabInfo"]["properties"]["label"]
            ["type"] = json!(["string", "integer"]);
        let error = validate_schema(&ambiguous_label, 20).expect_err("ambiguous label");
        assert!(error.to_string().contains("tab_info.label"));

        let mut missing_processes = compatible_schema(20);
        missing_processes["schemas"]["success_response"]["$defs"]["PaneProcessInfo"]["properties"]
            .as_object_mut()
            .expect("process properties")
            .remove("foreground_processes");
        let error = validate_schema(&missing_processes, 20).expect_err("missing processes");
        assert!(error.to_string().contains("foreground_processes"));

        let mut incompatible_argv = compatible_schema(20);
        incompatible_argv["schemas"]["success_response"]["$defs"]["PaneProcessInfoProcess"]["properties"]
            ["argv"]["type"] = json!("integer");
        let error = validate_schema(&incompatible_argv, 20).expect_err("incompatible argv");
        assert!(error.to_string().contains("foreground_processes[].argv"));
    }

    #[test]
    fn accepts_compatible_all_of_method_and_response_variants() {
        let mut schema = compatible_schema(21);
        let rename = schema["schemas"]["request"]["oneOf"][1].take();
        schema["schemas"]["request"]["oneOf"][1] = json!({
            "allOf": [
                {"properties": {"method": rename["properties"]["method"].clone()}, "required": ["method"]},
                {"properties": {"params": rename["properties"]["params"].clone()}, "required": ["params"], "type": "object"}
            ]
        });

        let snapshot =
            schema["schemas"]["success_response"]["$defs"]["ResponseResult"]["oneOf"][0].take();
        schema["schemas"]["success_response"]["$defs"]["ResponseResult"]["oneOf"][0] = json!({
            "allOf": [
                {"properties": {"type": snapshot["properties"]["type"].clone()}, "required": ["type"]},
                {"properties": {"snapshot": snapshot["properties"]["snapshot"].clone()}, "required": ["snapshot"]}
            ]
        });

        validate_schema(&schema, 21).expect("compatible allOf schema");
    }

    #[test]
    fn rejects_incompatible_one_of_requests_and_repeated_all_of_properties() {
        let mut one_of = compatible_schema(21);
        one_of["schemas"]["request"]["$defs"]["TabRenameParams"]["properties"]["label"] =
            json!({"oneOf": [{"type": "integer"}, {"type": "null"}]});
        let error = validate_schema(&one_of, 21).expect_err("label must accept string");
        assert!(error.to_string().contains("tab.rename.label"));

        let mut repeated_all_of = compatible_schema(21);
        repeated_all_of["schemas"]["request"]["$defs"]["TabRenameParams"] = json!({
            "allOf": [
                {"type": "object", "properties": {"tab_id": {"type": "string"}, "label": {"type": "string"}}, "required": ["tab_id", "label"]},
                {"properties": {"label": {"type": "integer"}}}
            ]
        });
        let error = validate_schema(&repeated_all_of, 21).expect_err("contradictory allOf label");
        assert!(error.to_string().contains("tab.rename.label"));

        let mut overlapping_one_of = compatible_schema(21);
        overlapping_one_of["schemas"]["request"]["$defs"]["TabRenameParams"]["properties"]["label"] =
            json!({"oneOf": [{"type": "string"}, {"type": ["string", "null"]}]});
        let error =
            validate_schema(&overlapping_one_of, 21).expect_err("overlapping request oneOf");
        assert!(error.to_string().contains("tab.rename.label"));

        let mut unknown_one_of = compatible_schema(21);
        unknown_one_of["schemas"]["request"]["$defs"]["TabRenameParams"]["properties"]["label"] = json!({
            "oneOf": [{"type": "string"}, {"not": {"type": "integer"}}]
        });
        let error = validate_schema(&unknown_one_of, 21).expect_err("unknown request oneOf");
        assert!(error.to_string().contains("tab.rename.label"));

        for constraint in [
            json!({"const": 7}),
            json!({"enum": [7]}),
            json!({"not": {"type": "string"}}),
        ] {
            let mut unsupported = compatible_schema(21);
            unsupported["schemas"]["request"]["$defs"]["TabRenameParams"]["properties"]["label"] =
                constraint;
            let error =
                validate_schema(&unsupported, 21).expect_err("unsupported request constraint");
            assert!(error.to_string().contains("tab.rename.label"));
        }

        let mut reference_sibling = compatible_schema(21);
        reference_sibling["schemas"]["request"]["$defs"]["TabRenameParams"]["properties"]["label"] = json!({
            "$ref": "#/schemas/request/$defs/StringValue",
            "not": {"type": "string"}
        });
        let error = validate_schema(&reference_sibling, 21).expect_err("$ref assertion sibling");
        assert!(error.to_string().contains("assertion siblings"));
    }

    #[test]
    fn accepts_nullable_one_of_responses_and_the_supported_ok_rename_result() {
        let mut schema = compatible_schema(21);
        schema["schemas"]["success_response"]["$defs"]["PaneProcessInfoProcess"]["properties"]["cmdline"] =
            json!({"oneOf": [{"type": "string"}, {"type": "null"}]});
        schema["schemas"]["success_response"]["$defs"]["ResponseResult"]["oneOf"]
            .as_array_mut()
            .expect("response variants")
            .retain(|variant| variant["properties"]["type"]["const"] != "tab_info");

        validate_schema(&schema, 21).expect("supported ok rename result");
    }

    #[test]
    fn rejects_overlapping_response_one_of_and_optional_rename_discriminator() {
        let mut overlapping = compatible_schema(21);
        overlapping["schemas"]["success_response"]["$defs"]["PaneProcessInfoProcess"]["properties"]
            ["cmdline"] = json!({
            "oneOf": [{"type": "string"}, {"type": ["string", "null"]}]
        });
        assert!(validate_schema(&overlapping, 21).is_err());

        let mut unknown_all_of = compatible_schema(21);
        unknown_all_of["schemas"]["success_response"]["$defs"]["PaneProcessInfoProcess"]["properties"]
            ["cmdline"] = json!({
            "allOf": [{"type": "string"}, {"not": {"type": "string"}}]
        });
        assert!(validate_schema(&unknown_all_of, 21).is_err());

        let mut optional_type = compatible_schema(21);
        let variants =
            optional_type["schemas"]["success_response"]["$defs"]["ResponseResult"]["oneOf"]
                .as_array_mut()
                .expect("response variants");
        variants.retain(|variant| variant["properties"]["type"]["const"] != "ok");
        let tab_info = variants
            .iter_mut()
            .find(|variant| variant["properties"]["type"]["const"] == "tab_info")
            .expect("tab_info variant");
        tab_info["required"] = json!(["tab"]);
        let error = validate_schema(&optional_type, 21).expect_err("optional discriminator");
        assert!(error.to_string().contains("tab.rename"));

        let mut unbounded_integer = compatible_schema(21);
        unbounded_integer["schemas"]["success_response"]["$defs"]["SessionSnapshot"]["properties"]
            ["protocol"] = json!({"type": "integer"});
        let error = validate_schema(&unbounded_integer, 21).expect_err("unsigned protocol");
        assert!(error.to_string().contains("integer formats"));

        let mut too_wide_protocol = compatible_schema(21);
        too_wide_protocol["schemas"]["success_response"]["$defs"]["SessionSnapshot"]["properties"]
            ["protocol"]["format"] = json!("uint64");
        let error = validate_schema(&too_wide_protocol, 21).expect_err("u64 protocol");
        assert!(error.to_string().contains("uint32"));
    }

    #[test]
    fn malformed_command_output_has_an_actionable_operation() {
        let error = parse_command_json("api schema --json", b"not-json")
            .expect_err("malformed command output");
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("api schema --json"));
        assert!(diagnostic.contains("invalid JSON"));
    }

    #[test]
    fn rejects_unresolved_references_and_protocol_contradictions() {
        let mut broken_reference = compatible_schema(20);
        broken_reference["schemas"]["request"]["oneOf"][0]["properties"]["params"]["$ref"] =
            json!("#/schemas/request/$defs/Missing");
        assert!(validate_schema(&broken_reference, 20).is_err());
        assert!(validate_schema(&compatible_schema(21), 20).is_err());
    }

    #[test]
    fn rejects_snapshot_status_contradiction() {
        let observed = ObservedContract {
            version: "0.8.2".to_string(),
            protocol: 20,
        };
        assert!(validate_snapshot_contract("0.8.3", 20, &observed).is_err());
        assert!(validate_snapshot_contract("0.8.2", 21, &observed).is_err());
    }
}
