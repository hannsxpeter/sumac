//! Deterministic local workflow checks for trusted project manifests.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use clap::ValueEnum;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Default per-check execution timeout in seconds.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
/// Maximum allowed per-check timeout in seconds.
pub const MAX_TIMEOUT_SECONDS: u64 = 3600;
/// Default per-stream (stdout and stderr) capture limit in bytes.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum allowed per-stream capture limit in bytes.
pub const MAX_ALLOWED_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_CHECKS_PER_MANIFEST: usize = 256;
const READ_CHUNK_BYTES: usize = 8192;
const REAP_WAIT_SECONDS: u64 = 2;

const MAX_VALUE_PREVIEW_BYTES: usize = 256;
const MAX_DIAGNOSTIC_VALUE_BYTES: usize = 128;
const MAX_CHECK_REASON_BYTES: usize = 512;
const MAX_MISMATCHES_PER_CHECK: usize = 32;
const MAX_SUITE_REPORT_BYTES: usize = 128 * 1024;

const ALLOWED_JSON_TYPES: [&str; 7] = [
    "null", "boolean", "number", "integer", "string", "array", "object",
];

/// Output format for `sxmc check`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum CheckOutputFormat {
    Human,
    Json,
}

/// Runtime options for executing a `.sxmc/checks.toml` manifest.
#[derive(Clone, Debug)]
pub struct CheckRunConfig {
    pub root: PathBuf,
    pub manifest: Option<PathBuf>,
    pub format: CheckOutputFormat,
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
}

/// Overall check suite status.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportStatus {
    Passed,
    Failed,
    Error,
}

/// Individual check outcome status.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Passed,
    Failed,
    Error,
}

/// Aggregate check counts in a report.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CheckSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub error: usize,
}

/// Expected assertions recorded for a single check.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ExpectedSummary {
    pub exit_code: i32,
    pub json_types: BTreeMap<String, String>,
    pub json_equals: BTreeMap<String, Value>,
}

/// Observed values recorded for a single check.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ActualSummary {
    pub exit_code: Option<i32>,
    pub json_parsed: Option<bool>,
    pub json_types: BTreeMap<String, String>,
    pub json_values: BTreeMap<String, Value>,
    pub missing_pointers: Vec<String>,
}

/// Structured mismatch entry explaining a failed expectation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CheckMismatch {
    pub kind: String,
    pub pointer: Option<String>,
    pub expected: Value,
    pub actual: Value,
    pub reason: String,
}

/// Per-check deterministic report entry.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CheckReportItem {
    pub name: String,
    pub status: CheckStatus,
    pub expected: ExpectedSummary,
    pub actual: ActualSummary,
    pub reason: String,
    pub mismatches: Vec<CheckMismatch>,
}

/// Top-level report returned by `sxmc check`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CheckReport {
    pub schema_version: u32,
    pub status: ReportStatus,
    pub exit_code: i32,
    pub root: String,
    pub manifest: String,
    pub truncated: bool,
    pub summary: CheckSummary,
    pub error: Option<String>,
    pub checks: Vec<CheckReportItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    version: Option<u32>,
    checks: Option<Vec<RawCheckSpec>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCheckSpec {
    name: Option<String>,
    argv: Option<Vec<String>>,
    #[serde(default)]
    expect_exit: i32,
    #[serde(default)]
    json_types: BTreeMap<String, String>,
    #[serde(default)]
    json_equals: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone)]
struct ValidatedCheck {
    name: String,
    argv: Vec<String>,
    expect_exit: i32,
    json_types: BTreeMap<String, (JsonPointer, String)>,
    json_equals: BTreeMap<String, (JsonPointer, Value)>,
}

/// Parsed strict RFC 6901 JSON Pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPointer {
    raw: String,
}

enum PointerLookup<'a> {
    Found(&'a Value),
    Missing,
}

impl JsonPointer {
    /// Parses and validates a strict RFC 6901 JSON Pointer.
    pub fn parse(raw: &str) -> std::result::Result<Self, String> {
        if raw.is_empty() {
            return Ok(Self { raw: String::new() });
        }
        if !raw.starts_with('/') {
            return Err(format!(
                "invalid JSON Pointer '{raw}': must be empty or start with '/'"
            ));
        }
        for part in raw[1..].split('/') {
            validate_pointer_escapes(part, raw)?;
        }
        Ok(Self {
            raw: raw.to_string(),
        })
    }

    fn lookup<'a>(&self, root: &'a Value) -> PointerLookup<'a> {
        match root.pointer(&self.raw) {
            Some(val) => PointerLookup::Found(val),
            None => PointerLookup::Missing,
        }
    }
}

fn validate_pointer_escapes(
    raw_token: &str,
    full_pointer: &str,
) -> std::result::Result<(), String> {
    let mut chars = raw_token.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0') | Some('1') => {}
                Some(other) => {
                    return Err(format!(
                        "invalid JSON Pointer escape '~{other}' in '{full_pointer}'"
                    ));
                }
                None => {
                    return Err(format!(
                        "incomplete JSON Pointer escape '~' in '{full_pointer}'"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Suite-wide budget bounding retained report values and diagnostics.
struct SuiteReportBudget {
    remaining_bytes: usize,
    truncated: bool,
}

impl SuiteReportBudget {
    fn new(limit: usize) -> Self {
        Self {
            remaining_bytes: limit,
            truncated: false,
        }
    }

    fn retain(&mut self, detail: &impl Serialize) -> bool {
        let mut writer = LimitedWriter::new(io::sink(), self.remaining_bytes);
        if serde_json::to_writer(&mut writer, detail).is_ok() {
            self.remaining_bytes = writer.remaining;
            true
        } else {
            self.truncated = true;
            false
        }
    }

    fn insert_text(&mut self, map: &mut BTreeMap<String, String>, key: &str, value: &str) {
        // A JSON pair covers the member, including its separator from adjacent members.
        if self.retain(&(key, value)) {
            map.insert(key.to_owned(), value.to_owned());
        }
    }

    fn insert_value(&mut self, map: &mut BTreeMap<String, Value>, key: &str, value: &Value) {
        if self.remaining_bytes == 0 {
            self.truncated = true;
            return;
        }
        let (value, _) = self.preview_value(value, MAX_VALUE_PREVIEW_BYTES);
        if self.retain(&(key, &value)) {
            map.insert(key.to_owned(), value.into_owned());
        }
    }

    fn push_pointer(&mut self, pointers: &mut Vec<String>, pointer: &str) {
        if self.retain(&[pointer]) {
            pointers.push(pointer.to_owned());
        }
    }

    fn preview_value<'a>(&mut self, value: &'a Value, limit: usize) -> (Cow<'a, Value>, String) {
        let (mut preview, cut) = format_bounded_json_preview(value, limit);
        if !cut {
            return (Cow::Borrowed(value), preview);
        }

        self.truncated = true;
        let marker = serde_json::json!({
            "_truncated": true,
            "preview": truncate_utf8_prefix(&preview, MAX_DIAGNOSTIC_VALUE_BYTES),
        });
        preview.push_str("...");
        (Cow::Owned(marker), preview)
    }

    fn clip_reason(&mut self, text: String, limit: usize) -> String {
        if text.len() <= limit {
            return text;
        }
        self.truncated = true;
        let suffix = " ... (truncated)";
        let prefix = truncate_utf8_prefix(&text, limit - suffix.len());
        format!("{prefix}{suffix}")
    }

    fn retain_reason(&mut self, text: String) -> String {
        self.retain_text(text, MAX_CHECK_REASON_BYTES)
            .unwrap_or_default()
    }

    fn retain_text(&mut self, text: String, limit: usize) -> Option<String> {
        let text = self.clip_reason(text, limit);
        self.retain(&text).then_some(text)
    }
}

struct LimitedWriter<W> {
    inner: W,
    remaining: usize,
}

impl<W> LimitedWriter<W> {
    fn new(inner: W, limit: usize) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let take = input.len().min(self.remaining);
        self.inner.write_all(&input[..take])?;
        self.remaining -= take;
        if take < input.len() {
            return Err(io::Error::other("JSON byte limit reached"));
        }
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn format_bounded_json_preview(val: &Value, max_bytes: usize) -> (String, bool) {
    let mut writer = LimitedWriter::new(Vec::new(), max_bytes);
    let truncated = serde_json::to_writer(&mut writer, val).is_err();
    let mut bytes = writer.inner;
    bytes.truncate(utf8_boundary_len(&bytes));
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (text, truncated)
}

fn truncate_utf8_prefix(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn utf8_boundary_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(err) => err.valid_up_to(),
    }
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor).map(Self)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any valid JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, v: bool) -> std::result::Result<Self::Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> std::result::Result<Self::Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> std::result::Result<Self::Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E> {
        Ok(Value::String(v.to_string()))
    }

    fn visit_string<E>(self, v: String) -> std::result::Result<Self::Value, E> {
        Ok(Value::String(v))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer).map(|v| v.0)
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::new();
        while let Some(elem) = seq.next_element::<StrictValue>()? {
            items.push(elem.0);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut obj = Map::new();
        while let Some((key, val)) = map.next_entry::<String, StrictValue>()? {
            if obj.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key '{key}'"
                )));
            }
            obj.insert(key, val.0);
        }
        Ok(Value::Object(obj))
    }
}

fn parse_strict_json(bytes: &[u8]) -> std::result::Result<Value, String> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let StrictValue(val) = StrictValue::deserialize(&mut de).map_err(|err| err.to_string())?;
    de.end().map_err(|err| err.to_string())?;
    Ok(val)
}

/// Runs `sxmc check` from CLI options, prints the report, and returns the exit code (`0`, `1`, or `2`).
pub async fn run_cli(config: CheckRunConfig) -> i32 {
    let report = execute_checks(&config).await;
    match config.format {
        CheckOutputFormat::Json => {
            let rendered = serde_json::to_string(&report).unwrap_or_else(|err| {
                format!(
                    "{{\"schema_version\":1,\"status\":\"error\",\"exit_code\":2,\"error\":\"{}\"}}",
                    err
                )
            });
            println!("{rendered}");
        }
        CheckOutputFormat::Human => {
            println!("{}", render_human_report(&report));
        }
    }
    report.exit_code
}

/// Validates inputs and executes all checks sequentially.
pub async fn execute_checks(config: &CheckRunConfig) -> CheckReport {
    let initial_manifest_display = config
        .manifest
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| {
            config
                .root
                .join(".sxmc")
                .join("checks.toml")
                .display()
                .to_string()
        });

    if let Err(err) = validate_knobs(config.timeout_seconds, config.max_output_bytes) {
        return CheckReport::preflight_error(
            config.root.display().to_string(),
            initial_manifest_display,
            err,
        );
    }

    let canonical_root = match resolve_and_validate_root(&config.root) {
        Ok(path) => path,
        Err(err) => {
            return CheckReport::preflight_error(
                config.root.display().to_string(),
                initial_manifest_display,
                err,
            );
        }
    };

    let manifest_path = config
        .manifest
        .clone()
        .unwrap_or_else(|| canonical_root.join(".sxmc").join("checks.toml"));
    let manifest_display = manifest_path.display().to_string();

    let validated_checks = match load_and_validate_manifest(&manifest_path) {
        Ok(checks) => checks,
        Err(err) => {
            return CheckReport::preflight_error(
                canonical_root.display().to_string(),
                manifest_display,
                err,
            );
        }
    };

    let timeout = Duration::from_secs(config.timeout_seconds);
    let mut budget = SuiteReportBudget::new(MAX_SUITE_REPORT_BYTES);
    let mut results = Vec::with_capacity(validated_checks.len());
    for check in &validated_checks {
        let item = run_single_check(
            check,
            &canonical_root,
            timeout,
            config.max_output_bytes,
            &mut budget,
        )
        .await;
        results.push(item);
    }

    CheckReport::from_results(
        canonical_root.display().to_string(),
        manifest_display,
        budget.truncated,
        results,
    )
}

impl CheckReport {
    fn preflight_error(root: String, manifest: String, error: String) -> Self {
        let mut budget = SuiteReportBudget::new(MAX_SUITE_REPORT_BYTES);
        let error = if budget.retain(&error) {
            Some(error)
        } else {
            // JSON can use six bytes for one input byte, plus quotes for the string.
            budget.retain_text(error, (MAX_SUITE_REPORT_BYTES - 2) / 6)
        };
        Self {
            schema_version: 1,
            status: ReportStatus::Error,
            exit_code: 2,
            root,
            manifest,
            truncated: budget.truncated,
            summary: CheckSummary::default(),
            error,
            checks: Vec::new(),
        }
    }

    fn from_results(
        root: String,
        manifest: String,
        truncated: bool,
        checks: Vec<CheckReportItem>,
    ) -> Self {
        let mut summary = CheckSummary {
            total: checks.len(),
            ..CheckSummary::default()
        };
        for item in &checks {
            match item.status {
                CheckStatus::Passed => summary.passed += 1,
                CheckStatus::Failed => summary.failed += 1,
                CheckStatus::Error => summary.error += 1,
            }
        }
        // Exit code 2 (runner/manifest error) wins over exit code 1 (expectation mismatch).
        let (status, exit_code) = if summary.error > 0 {
            (ReportStatus::Error, 2)
        } else if summary.failed > 0 {
            (ReportStatus::Failed, 1)
        } else {
            (ReportStatus::Passed, 0)
        };
        Self {
            schema_version: 1,
            status,
            exit_code,
            root,
            manifest,
            truncated,
            summary,
            error: None,
            checks,
        }
    }
}

fn validate_knobs(
    timeout_seconds: u64,
    max_output_bytes: usize,
) -> std::result::Result<(), String> {
    if timeout_seconds == 0 || timeout_seconds > MAX_TIMEOUT_SECONDS {
        return Err(format!(
            "invalid timeout-seconds {timeout_seconds}: must be between 1 and {MAX_TIMEOUT_SECONDS}"
        ));
    }
    if max_output_bytes == 0 || max_output_bytes > MAX_ALLOWED_OUTPUT_BYTES {
        return Err(format!(
            "invalid max-output-bytes {max_output_bytes}: must be between 1 and {MAX_ALLOWED_OUTPUT_BYTES}"
        ));
    }
    Ok(())
}

fn resolve_and_validate_root(root: &Path) -> std::result::Result<PathBuf, String> {
    let metadata = fs::metadata(root)
        .map_err(|err| format!("invalid root directory '{}': {err}", root.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "invalid root '{}': not a directory",
            root.display()
        ));
    }
    fs::canonicalize(root)
        .map_err(|err| format!("failed to canonicalize root '{}': {err}", root.display()))
}

fn load_and_validate_manifest(path: &Path) -> std::result::Result<Vec<ValidatedCheck>, String> {
    let metadata = fs::metadata(path)
        .map_err(|err| format!("unable to read manifest '{}': {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "manifest '{}' is not a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "manifest '{}' exceeds maximum size of {} bytes",
            path.display(),
            MAX_MANIFEST_BYTES
        ));
    }
    let raw = fs::read_to_string(path)
        .map_err(|err| format!("failed to read manifest '{}': {err}", path.display()))?;
    parse_and_validate_manifest(&raw)
}

fn parse_and_validate_manifest(raw: &str) -> std::result::Result<Vec<ValidatedCheck>, String> {
    let parsed: RawManifest =
        toml::from_str(raw).map_err(|err| format!("invalid manifest TOML: {err}"))?;

    let mut errors = Vec::new();
    match parsed.version {
        Some(1) => {}
        Some(other) => errors.push(format!("unsupported manifest version {other}; expected 1")),
        None => errors.push("missing required manifest field 'version'".to_string()),
    }

    let raw_checks = match parsed.checks {
        Some(list) => list,
        None => {
            errors.push("missing required '[[checks]]' entries".to_string());
            return Err(errors.join("; "));
        }
    };

    if raw_checks.is_empty() {
        errors.push("manifest must define at least one check in [[checks]]".to_string());
    } else if raw_checks.len() > MAX_CHECKS_PER_MANIFEST {
        errors.push(format!(
            "manifest defines {} checks, exceeding maximum of {}",
            raw_checks.len(),
            MAX_CHECKS_PER_MANIFEST
        ));
    }

    let mut seen_names = HashSet::new();
    let mut validated = Vec::with_capacity(raw_checks.len());
    for (idx, raw_check) in raw_checks.into_iter().enumerate() {
        match validate_check_spec(idx, raw_check, &mut seen_names) {
            Ok(check) => validated.push(check),
            Err(mut check_errors) => errors.append(&mut check_errors),
        }
    }

    if errors.is_empty() {
        Ok(validated)
    } else {
        Err(format!("invalid manifest: {}", errors.join("; ")))
    }
}

fn validate_check_spec(
    index: usize,
    raw: RawCheckSpec,
    seen_names: &mut HashSet<String>,
) -> std::result::Result<ValidatedCheck, Vec<String>> {
    let mut errors = Vec::new();
    let label = raw
        .name
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .map(|n| format!("check '{n}'"))
        .unwrap_or_else(|| format!("checks[{index}]"));

    let name = match raw.name {
        Some(n) => {
            if let Err(err) = validate_check_name(&n) {
                errors.push(format!("{label}: {err}"));
            } else if !seen_names.insert(n.clone()) {
                errors.push(format!("duplicate check name '{n}'"));
            }
            n
        }
        None => {
            errors.push(format!("{label}: missing required field 'name'"));
            String::new()
        }
    };

    let argv = match raw.argv {
        Some(args) => {
            if args.is_empty() {
                errors.push(format!("{label}: 'argv' must not be empty"));
            } else {
                if args[0].trim().is_empty() {
                    errors.push(format!("{label}: 'argv[0]' executable must not be empty"));
                } else if cfg!(windows) && is_windows_batch_entrypoint(&args[0]) {
                    errors.push(format!(
                        "{label}: Windows batch entry point '{}' (.bat/.cmd) is not allowed because cmd.exe reinterprets arguments",
                        args[0]
                    ));
                }
                for (arg_idx, arg) in args.iter().enumerate() {
                    if arg.contains('\0') {
                        errors.push(format!(
                            "{label}: 'argv[{arg_idx}]' contains an invalid NUL byte"
                        ));
                    }
                }
            }
            args
        }
        None => {
            errors.push(format!("{label}: missing required field 'argv'"));
            Vec::new()
        }
    };

    if !(0..=255).contains(&raw.expect_exit) {
        errors.push(format!(
            "{label}: invalid expect_exit {}; must be in 0..=255",
            raw.expect_exit
        ));
    }

    let mut json_types = BTreeMap::new();
    for (ptr_str, type_name) in raw.json_types {
        match JsonPointer::parse(&ptr_str) {
            Ok(ptr) => {
                if !ALLOWED_JSON_TYPES.contains(&type_name.as_str()) {
                    errors.push(format!(
                        "{label}: invalid json_types value '{type_name}' for '{ptr_str}'; expected one of {}",
                        ALLOWED_JSON_TYPES.join(", ")
                    ));
                } else {
                    json_types.insert(ptr_str, (ptr, type_name));
                }
            }
            Err(err) => errors.push(format!("{label}: {err}")),
        }
    }

    let mut json_equals = BTreeMap::new();
    for (ptr_str, toml_val) in raw.json_equals {
        match JsonPointer::parse(&ptr_str) {
            Ok(ptr) => match toml_to_json(&toml_val) {
                Ok(json_val) => {
                    json_equals.insert(ptr_str, (ptr, json_val));
                }
                Err(err) => errors.push(format!("{label}: json_equals['{ptr_str}']: {err}")),
            },
            Err(err) => errors.push(format!("{label}: {err}")),
        }
    }

    if errors.is_empty() {
        Ok(ValidatedCheck {
            name,
            argv,
            expect_exit: raw.expect_exit,
            json_types,
            json_equals,
        })
    } else {
        Err(errors)
    }
}

fn validate_check_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("check name must not be empty".to_string());
    }
    if name.trim() != name {
        return Err("check name must not have leading or trailing whitespace".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(
            "check name may only contain ASCII letters, digits, '-', '_', and '.'".to_string(),
        );
    }
    if !name.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err("check name must contain at least one ASCII letter or digit".to_string());
    }
    Ok(())
}

/// Rejects `.bat` and `.cmd` entry points across paths and casing so Windows `cmd.exe` wrapping never breaks literal `argv`.
fn is_windows_batch_entrypoint(exe: &str) -> bool {
    let trimmed = exe.trim();
    let filename = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let normalized = filename.trim_end_matches([' ', '.']).to_ascii_lowercase();
    normalized.ends_with(".bat") || normalized.ends_with(".cmd")
}

fn toml_to_json(val: &toml::Value) -> std::result::Result<Value, String> {
    match val {
        toml::Value::String(s) => Ok(Value::String(s.clone())),
        toml::Value::Integer(i) => Ok(Value::Number((*i).into())),
        toml::Value::Float(_) => Err(
            "floating-point values are not supported in json_equals (use json_types = \"number\" instead)"
                .to_string(),
        ),
        toml::Value::Boolean(b) => Ok(Value::Bool(*b)),
        toml::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(toml_to_json(item)?);
            }
            Ok(Value::Array(out))
        }
        toml::Value::Table(tbl) => {
            let mut map = Map::new();
            for (k, v) in tbl {
                map.insert(k.clone(), toml_to_json(v)?);
            }
            Ok(Value::Object(map))
        }
        toml::Value::Datetime(_) => {
            Err("TOML datetime values are not supported in json_equals".to_string())
        }
    }
}

struct BoundedChildOutput {
    exit_status: ExitStatus,
    stdout: Vec<u8>,
}

enum BoundedExecError {
    SpawnFailed(String),
    TimedOut {
        secs: u64,
        cleanup_error: Option<String>,
    },
    OutputLimitExceeded {
        stream: &'static str,
        limit: usize,
        cleanup_error: Option<String>,
    },
    IoFailure {
        detail: String,
        cleanup_error: Option<String>,
    },
}

async fn run_single_check(
    spec: &ValidatedCheck,
    root: &Path,
    timeout: Duration,
    max_output_bytes: usize,
    budget: &mut SuiteReportBudget,
) -> CheckReportItem {
    let expected = build_expected_summary(spec, budget);
    match execute_bounded_process(&spec.argv, root, timeout, max_output_bytes).await {
        Ok(output) => evaluate_completed_check(spec, expected, output, budget),
        Err(err) => {
            let raw_reason = match err {
                BoundedExecError::SpawnFailed(msg) => {
                    let executable = truncate_utf8_prefix(&spec.argv[0], MAX_CHECK_REASON_BYTES);
                    format!("failed to spawn '{executable}': {msg}")
                }
                BoundedExecError::TimedOut {
                    secs,
                    cleanup_error,
                } => {
                    append_cleanup_error(format!("command timed out after {secs}s"), cleanup_error)
                }
                BoundedExecError::OutputLimitExceeded {
                    stream,
                    limit,
                    cleanup_error,
                } => append_cleanup_error(
                    format!("{stream} exceeded limit of {limit} bytes"),
                    cleanup_error,
                ),
                BoundedExecError::IoFailure {
                    detail,
                    cleanup_error,
                } => append_cleanup_error(format!("process I/O error: {detail}"), cleanup_error),
            };
            CheckReportItem {
                name: spec.name.clone(),
                status: CheckStatus::Error,
                expected,
                actual: ActualSummary::default(),
                reason: budget.retain_reason(raw_reason),
                mismatches: Vec::new(),
            }
        }
    }
}

fn append_cleanup_error(base: String, cleanup_error: Option<String>) -> String {
    match cleanup_error {
        Some(err) => format!("{base} ({err})"),
        None => base,
    }
}

fn build_expected_summary(
    spec: &ValidatedCheck,
    budget: &mut SuiteReportBudget,
) -> ExpectedSummary {
    let mut json_types = BTreeMap::new();
    for (key, (_, value)) in &spec.json_types {
        budget.insert_text(&mut json_types, key, value);
    }

    let mut json_equals = BTreeMap::new();
    for (key, (_, value)) in &spec.json_equals {
        budget.insert_value(&mut json_equals, key, value);
    }

    ExpectedSummary {
        exit_code: spec.expect_exit,
        json_types,
        json_equals,
    }
}

async fn execute_bounded_process(
    argv: &[String],
    root: &Path,
    timeout: Duration,
    max_output_bytes: usize,
) -> std::result::Result<BoundedChildOutput, BoundedExecError> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|err| BoundedExecError::SpawnFailed(err.to_string()))?;
    run_bounded_child(&mut child, timeout, max_output_bytes).await
}

async fn run_bounded_child(
    child: &mut tokio::process::Child,
    timeout: Duration,
    max_output_bytes: usize,
) -> std::result::Result<BoundedChildOutput, BoundedExecError> {
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| BoundedExecError::IoFailure {
            detail: "missing piped stdout".to_string(),
            cleanup_error: None,
        })?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| BoundedExecError::IoFailure {
            detail: "missing piped stderr".to_string(),
            cleanup_error: None,
        })?;

    let mut stdout_buf = Vec::new();
    let mut stderr_len: usize = 0;
    let mut out_chunk = [0u8; READ_CHUNK_BYTES];
    let mut err_chunk = [0u8; READ_CHUNK_BYTES];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut exit_status: Option<ExitStatus> = None;

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    while !stdout_done || !stderr_done || exit_status.is_none() {
        tokio::select! {
            _ = &mut deadline => {
                let cleanup_error = terminate_and_reap(child).await.err();
                return Err(BoundedExecError::TimedOut {
                    secs: timeout.as_secs(),
                    cleanup_error,
                });
            }
            res = stdout_pipe.read(&mut out_chunk), if !stdout_done => {
                match res {
                    Ok(0) => stdout_done = true,
                    Ok(n) => {
                        if stdout_buf.len().saturating_add(n) > max_output_bytes {
                            let cleanup_error = terminate_and_reap(child).await.err();
                            return Err(BoundedExecError::OutputLimitExceeded {
                                stream: "stdout",
                                limit: max_output_bytes,
                                cleanup_error,
                            });
                        }
                        stdout_buf.extend_from_slice(&out_chunk[..n]);
                    }
                    Err(err) => {
                        let cleanup_error = terminate_and_reap(child).await.err();
                        return Err(BoundedExecError::IoFailure {
                            detail: format!("stdout read: {err}"),
                            cleanup_error,
                        });
                    }
                }
            }
            res = stderr_pipe.read(&mut err_chunk), if !stderr_done => {
                match res {
                    Ok(0) => stderr_done = true,
                    Ok(n) => {
                        if stderr_len.saturating_add(n) > max_output_bytes {
                            let cleanup_error = terminate_and_reap(child).await.err();
                            return Err(BoundedExecError::OutputLimitExceeded {
                                stream: "stderr",
                                limit: max_output_bytes,
                                cleanup_error,
                            });
                        }
                        stderr_len = stderr_len.saturating_add(n);
                    }
                    Err(err) => {
                        let cleanup_error = terminate_and_reap(child).await.err();
                        return Err(BoundedExecError::IoFailure {
                            detail: format!("stderr read: {err}"),
                            cleanup_error,
                        });
                    }
                }
            }
            status = child.wait(), if exit_status.is_none() => {
                match status {
                    Ok(status) => exit_status = Some(status),
                    Err(err) => {
                        let cleanup_error = terminate_and_reap(child).await.err();
                        return Err(BoundedExecError::IoFailure {
                            detail: format!("child wait: {err}"),
                            cleanup_error,
                        });
                    }
                }
            }
        }
    }

    Ok(BoundedChildOutput {
        exit_status: exit_status.expect("loop exits only once exit_status is Some"),
        stdout: stdout_buf,
    })
}

async fn terminate_and_reap(child: &mut tokio::process::Child) -> std::result::Result<(), String> {
    if let Err(err) = child.start_kill() {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return Ok(());
        }
        return Err(format!("failed to signal child process: {err}"));
    }
    reap_child_with_deadline(child, Duration::from_secs(REAP_WAIT_SECONDS)).await
}

async fn reap_child_with_deadline(
    child: &mut tokio::process::Child,
    reap_timeout: Duration,
) -> std::result::Result<(), String> {
    match tokio::time::timeout(reap_timeout, child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(format!("failed to reap child process: {err}")),
        Err(_) => Err(format!(
            "timed out after {}ms waiting to reap child process",
            reap_timeout.as_millis()
        )),
    }
}

fn evaluate_completed_check(
    spec: &ValidatedCheck,
    expected: ExpectedSummary,
    output: BoundedChildOutput,
    budget: &mut SuiteReportBudget,
) -> CheckReportItem {
    let actual_exit = output.exit_status.code().unwrap_or(-1);
    let mut actual = ActualSummary {
        exit_code: Some(actual_exit),
        ..ActualSummary::default()
    };
    let mut mismatches = MismatchLog::default();

    if actual_exit != spec.expect_exit {
        mismatches.record(budget, |_| MismatchDetail {
            kind: "exit_code",
            pointer: None,
            expected: Cow::Owned(Value::from(spec.expect_exit)),
            actual: Cow::Owned(Value::from(actual_exit)),
            reason: format!(
                "exit code mismatch: expected {}, got {}",
                spec.expect_exit, actual_exit
            ),
        });
    }

    if !spec.json_types.is_empty() || !spec.json_equals.is_empty() {
        match parse_strict_json(&output.stdout) {
            Ok(doc) => {
                actual.json_parsed = Some(true);
                evaluate_json_types(spec, &doc, &mut actual, &mut mismatches, budget);
                evaluate_json_equals(spec, &doc, &mut actual, &mut mismatches, budget);
            }
            Err(err) => {
                actual.json_parsed = Some(false);
                mismatches.record(budget, |_| MismatchDetail {
                    kind: "invalid_json",
                    pointer: None,
                    expected: Cow::Owned(Value::from("valid JSON on stdout")),
                    actual: Cow::Owned(Value::from(format!(
                        "invalid JSON ({} bytes)",
                        output.stdout.len()
                    ))),
                    reason: format!("stdout is not valid JSON: {err}"),
                });
            }
        }
    }

    let (status, reason) = if !mismatches.failed {
        (CheckStatus::Passed, "all assertions passed".to_owned())
    } else if mismatches.entries.is_empty() {
        (
            CheckStatus::Failed,
            "assertions failed (details omitted)".to_owned(),
        )
    } else {
        let joined = mismatches
            .entries
            .iter()
            .map(|m| m.reason.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        (CheckStatus::Failed, joined)
    };
    CheckReportItem {
        name: spec.name.clone(),
        status,
        expected,
        actual,
        reason: budget.retain_reason(reason),
        mismatches: mismatches.entries,
    }
}

// Borrow input fields while checking whether the complete entry fits.
#[derive(Serialize)]
struct MismatchDetail<'a> {
    kind: &'static str,
    pointer: Option<&'a str>,
    expected: Cow<'a, Value>,
    actual: Cow<'a, Value>,
    reason: String,
}

#[derive(Default)]
struct MismatchLog {
    failed: bool,
    entries: Vec<CheckMismatch>,
}

impl MismatchLog {
    fn record<'a>(
        &mut self,
        budget: &mut SuiteReportBudget,
        detail: impl FnOnce(&mut SuiteReportBudget) -> MismatchDetail<'a>,
    ) {
        self.failed = true;
        if budget.remaining_bytes == 0 || self.entries.len() > MAX_MISMATCHES_PER_CHECK {
            budget.truncated = true;
            return;
        }

        let mut detail = if self.entries.len() == MAX_MISMATCHES_PER_CHECK {
            budget.truncated = true;
            MismatchDetail {
                kind: "truncated",
                pointer: None,
                expected: Cow::Owned(Value::Null),
                actual: Cow::Owned(Value::Null),
                reason: format!(
                    "additional mismatches omitted after {MAX_MISMATCHES_PER_CHECK} entries"
                ),
            }
        } else {
            detail(budget)
        };
        detail.reason = budget.clip_reason(detail.reason, MAX_CHECK_REASON_BYTES);
        if budget.retain(&[&detail]) {
            self.entries.push(CheckMismatch {
                kind: detail.kind.to_owned(),
                pointer: detail.pointer.map(str::to_owned),
                expected: detail.expected.into_owned(),
                actual: detail.actual.into_owned(),
                reason: detail.reason,
            });
        }
    }
}

fn evaluate_json_types(
    spec: &ValidatedCheck,
    doc: &Value,
    actual: &mut ActualSummary,
    mismatches: &mut MismatchLog,
    budget: &mut SuiteReportBudget,
) {
    for (key, (pointer, expected_type)) in &spec.json_types {
        let actual_type = match pointer.lookup(doc) {
            PointerLookup::Missing => {
                budget.push_pointer(&mut actual.missing_pointers, key);
                "missing"
            }
            PointerLookup::Found(value) => json_type_name(value),
        };
        budget.insert_text(&mut actual.json_types, key, actual_type);
        if actual_type == expected_type || (expected_type == "number" && actual_type == "integer") {
            continue;
        }

        mismatches.record(budget, |_| {
            let key_prefix = truncate_utf8_prefix(key, MAX_CHECK_REASON_BYTES);
            let reason = if actual_type == "missing" {
                format!("JSON pointer '{key_prefix}' is missing (expected type '{expected_type}')")
            } else {
                format!(
                    "JSON pointer '{key_prefix}' type mismatch: expected '{expected_type}', got '{actual_type}'"
                )
            };
            MismatchDetail {
                kind: "json_type",
                pointer: Some(key),
                expected: Cow::Owned(Value::from(expected_type.as_str())),
                actual: Cow::Owned(Value::from(actual_type)),
                reason,
            }
        });
    }
}

fn evaluate_json_equals(
    spec: &ValidatedCheck,
    doc: &Value,
    actual: &mut ActualSummary,
    mismatches: &mut MismatchLog,
    budget: &mut SuiteReportBudget,
) {
    for (key, (pointer, expected)) in &spec.json_equals {
        let observed = match pointer.lookup(doc) {
            PointerLookup::Missing => {
                if !spec.json_types.contains_key(key) {
                    budget.push_pointer(&mut actual.missing_pointers, key);
                }
                None
            }
            PointerLookup::Found(value) => {
                budget.insert_value(&mut actual.json_values, key, value);
                if value == expected {
                    continue;
                }
                Some(value)
            }
        };

        mismatches.record(budget, |budget| {
            let key_prefix = truncate_utf8_prefix(key, MAX_CHECK_REASON_BYTES);
            let (expected, expected_preview) =
                budget.preview_value(expected, MAX_DIAGNOSTIC_VALUE_BYTES);
            let (actual, reason) = match observed {
                Some(value) => {
                    let (actual, actual_preview) =
                        budget.preview_value(value, MAX_DIAGNOSTIC_VALUE_BYTES);
                    let reason = format!(
                        "JSON pointer '{key_prefix}' mismatch: expected {expected_preview}, got {actual_preview}"
                    );
                    (actual, reason)
                }
                None => (
                    Cow::Owned(Value::from("missing")),
                    format!("JSON pointer '{key_prefix}' is missing (expected {expected_preview})"),
                ),
            };
            MismatchDetail {
                kind: "json_equals",
                pointer: Some(key),
                expected,
                actual,
                reason,
            }
        });
    }
}

fn json_type_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Renders a deterministic human-readable report using ASCII punctuation only.
pub fn render_human_report(report: &CheckReport) -> String {
    let status_label = match report.status {
        ReportStatus::Passed => "PASS",
        ReportStatus::Failed => "FAIL",
        ReportStatus::Error => "ERROR",
    };

    let mut lines = vec![
        format!(
            "sxmc check: {status_label} ({} passed, {} failed, {} error, {} total; exit {})",
            report.summary.passed,
            report.summary.failed,
            report.summary.error,
            report.summary.total,
            report.exit_code
        ),
        format!("Root: {}", report.root),
        format!("Manifest: {}", report.manifest),
    ];

    if report.truncated {
        lines.push("Truncated: true (report output exceeded retention budget)".to_string());
    }

    if let Some(err) = &report.error {
        lines.push(format!("Reason: {err}"));
        return lines.join("\n");
    }

    for item in &report.checks {
        let tag = match item.status {
            CheckStatus::Passed => "PASS",
            CheckStatus::Failed => "FAIL",
            CheckStatus::Error => "ERROR",
        };
        lines.push(format!("[{tag}] {}: {}", item.name, item.reason));
        lines.push(format!(
            "  expected: {}",
            format_expected_line(&item.expected)
        ));
        lines.push(format!("  actual:   {}", format_actual_line(&item.actual)));
    }

    lines.join("\n")
}

fn format_expected_line(expected: &ExpectedSummary) -> String {
    let mut parts = vec![format!("exit={}", expected.exit_code)];
    for (ptr, ty) in &expected.json_types {
        parts.push(format!("type({ptr})={ty}"));
    }
    for (ptr, val) in &expected.json_equals {
        let (preview, cut) = format_bounded_json_preview(val, MAX_DIAGNOSTIC_VALUE_BYTES);
        if cut {
            parts.push(format!("{ptr}={preview}..."));
        } else {
            parts.push(format!("{ptr}={preview}"));
        }
    }
    parts.join(", ")
}

fn format_actual_line(actual: &ActualSummary) -> String {
    let mut parts = Vec::new();
    match actual.exit_code {
        Some(code) => parts.push(format!("exit={code}")),
        None => parts.push("exit=<not completed>".to_string()),
    }
    if let Some(false) = actual.json_parsed {
        parts.push("json=invalid".to_string());
    }
    for (ptr, ty) in &actual.json_types {
        parts.push(format!("type({ptr})={ty}"));
    }
    for (ptr, val) in &actual.json_values {
        let (preview, cut) = format_bounded_json_preview(val, MAX_DIAGNOSTIC_VALUE_BYTES);
        if cut {
            parts.push(format!("{ptr}={preview}..."));
        } else {
            parts.push(format!("{ptr}={preview}"));
        }
    }
    for ptr in &actual.missing_pointers {
        if !actual.json_types.contains_key(ptr) {
            parts.push(format!("{ptr}=<missing>"));
        }
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_json_pointer_parses_root_and_escapes() {
        let root = JsonPointer::parse("").unwrap();
        let doc = serde_json::json!({
            "a/b": { "m~n": [10, 20] },
            "": "empty-key",
            "nil": null
        });
        assert!(matches!(root.lookup(&doc), PointerLookup::Found(v) if v == &doc));

        let escaped = JsonPointer::parse("/a~1b/m~0n/1").unwrap();
        assert!(
            matches!(escaped.lookup(&doc), PointerLookup::Found(v) if v == &serde_json::json!(20))
        );

        let empty_key = JsonPointer::parse("/").unwrap();
        assert!(
            matches!(empty_key.lookup(&doc), PointerLookup::Found(v) if v == &serde_json::json!("empty-key"))
        );

        let leading_zero = JsonPointer::parse("/a~1b/m~0n/01").unwrap();
        assert!(matches!(leading_zero.lookup(&doc), PointerLookup::Missing));
    }

    #[test]
    fn strict_json_pointer_rejects_invalid_syntax() {
        assert!(JsonPointer::parse("no-slash").is_err());
        assert!(JsonPointer::parse("/bad~2").is_err());
        assert!(JsonPointer::parse("/trailing~").is_err());
    }

    #[test]
    fn manifest_validation_collects_errors_and_rejects_toml_datetime_and_floats() {
        let raw_dt = r#"
version = 1

[[checks]]
name = "dt"
argv = ["echo", "hi"]
[checks.json_equals]
"/ts" = 2026-01-01T00:00:00Z
"#;
        let err_dt = parse_and_validate_manifest(raw_dt).unwrap_err();
        assert!(err_dt.contains("datetime"), "unexpected error: {err_dt}");

        let raw_float = r#"
version = 1

[[checks]]
name = "flt"
argv = ["echo", "hi"]
[checks.json_equals]
"/nested" = { val = 1.25 }
"#;
        let err_flt = parse_and_validate_manifest(raw_float).unwrap_err();
        assert!(
            err_flt.contains("floating-point"),
            "unexpected error: {err_flt}"
        );
    }

    #[test]
    fn strict_json_parser_rejects_duplicate_object_members() {
        let err = parse_strict_json(br#"{"a": 0, "a": 1}"#).unwrap_err();
        assert!(err.contains("duplicate"), "unexpected error: {err}");

        let nested_err = parse_strict_json(br#"[{"k": 1, "k": 2}]"#).unwrap_err();
        assert!(
            nested_err.contains("duplicate"),
            "unexpected error: {nested_err}"
        );

        let ok = parse_strict_json(br#"{"a": 1, "b": {"a": 2}}"#).unwrap();
        assert_eq!(ok["a"], 1);
        assert_eq!(ok["b"]["a"], 2);
    }

    #[test]
    fn windows_batch_entrypoint_detection_covers_paths_and_case() {
        assert!(is_windows_batch_entrypoint("run.bat"));
        assert!(is_windows_batch_entrypoint("RUN.CMD"));
        assert!(is_windows_batch_entrypoint("./scripts/Tool.Bat"));
        assert!(is_windows_batch_entrypoint(r"C:\bin\job.cmd"));
        assert!(is_windows_batch_entrypoint("job.bat."));
        assert!(!is_windows_batch_entrypoint("bat"));
        assert!(!is_windows_batch_entrypoint("python3"));
        assert!(!is_windows_batch_entrypoint(
            "./scripts/list_failed_jobs.py"
        ));
    }

    #[test]
    fn report_budget_admits_exact_fit_and_flags_the_next_omission() {
        let key = "\u{1}";
        let wire_bytes = serde_json::to_vec(&(key, 1)).unwrap().len();
        let mut budget = SuiteReportBudget::new(wire_bytes);
        let mut values = BTreeMap::new();
        budget.insert_value(&mut values, key, &Value::from(1));
        assert_eq!(values, BTreeMap::from([(key.to_owned(), Value::from(1))]));
        assert_eq!(budget.remaining_bytes, 0);
        assert!(!budget.truncated);

        budget.insert_value(&mut values, "/extra", &Value::from(2));
        assert!(budget.truncated);
        assert_eq!(values, BTreeMap::from([(key.to_owned(), Value::from(1))]));
    }

    #[test]
    fn report_budget_rejects_an_entry_without_charging_its_partial_bytes() {
        let mut budget = SuiteReportBudget::new(32);
        let mut values = BTreeMap::new();
        budget.insert_value(&mut values, &"\u{1}".repeat(32), &Value::from(1));
        assert!(values.is_empty());
        assert_eq!(budget.remaining_bytes, 32);
        assert!(budget.truncated);

        budget.insert_value(&mut values, "/small", &Value::from(2));
        assert_eq!(
            values,
            BTreeMap::from([("/small".to_owned(), Value::from(2))])
        );
    }

    #[test]
    fn suite_report_budget_bounds_large_value_previews() {
        let mut budget = SuiteReportBudget::new(1024);
        let big = Value::String("\n".repeat(50_000));
        let mut values = BTreeMap::new();
        budget.insert_value(&mut values, "", &big);
        assert!(budget.truncated);
        let marker = &values[""];
        assert_eq!(marker["_truncated"], true);
        assert!(marker["preview"].as_str().unwrap().starts_with(r#""\n"#));
        let wire_bytes = serde_json::to_vec(&("", marker)).unwrap().len();
        assert!(wire_bytes < 512, "retained len={wire_bytes}");
        assert_eq!(budget.remaining_bytes, 1024 - wire_bytes);
    }

    async fn spawn_ready_blocking_child() -> tokio::process::Child {
        let mut python = None;
        for candidate in ["python3", "python"] {
            let mut probe = Command::new(candidate);
            probe
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let status = tokio::time::timeout(Duration::from_secs(5), probe.status())
                .await
                .expect("timed out probing Python interpreter");
            if status.is_ok_and(|status| status.success()) {
                python = Some(candidate);
                break;
            }
        }

        let mut cmd = Command::new(python.expect("python3 or python required"));
        cmd.args([
            "-c",
            "import sys; sys.stdout.write('ready'); sys.stdout.flush(); sys.stdin.read()",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

        let mut child = cmd.spawn().expect("spawn ready child");
        let stdout = child.stdout.as_mut().expect("piped stdout");
        let mut ready = [0; 5];
        tokio::time::timeout(Duration::from_secs(5), stdout.read_exact(&mut ready))
            .await
            .expect("timed out waiting for child readiness signal")
            .expect("read readiness signal");
        assert_eq!(&ready, b"ready");
        child
    }

    #[tokio::test]
    async fn runner_timeout_reaps_ready_child_before_returning() {
        let mut child = spawn_ready_blocking_child().await;
        let _stdin_hold = child.stdin.take().expect("hold child open until timeout");
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_bounded_child(&mut child, Duration::ZERO, 1024),
        )
        .await
        .expect("runner did not return after timeout");
        assert!(matches!(
            result,
            Err(BoundedExecError::TimedOut {
                secs: 0,
                cleanup_error: None,
            })
        ));
        // Tokio only clears the ID when the child has been polled to completion.
        assert_eq!(child.id(), None, "runner returned before reaping its child");
    }

    #[tokio::test]
    async fn reap_child_deadline_reports_a_live_child() {
        let mut child = spawn_ready_blocking_child().await;
        let _stdin_hold = child.stdin.take().expect("hold child open during reap");
        let err = reap_child_with_deadline(&mut child, Duration::ZERO)
            .await
            .expect_err("live child should hit reap deadline");
        assert!(err.contains("timed out"), "unexpected reap error: {err}");
        terminate_and_reap(&mut child)
            .await
            .expect("cleanup after reap-deadline test");
        assert_eq!(child.id(), None);
    }
}
