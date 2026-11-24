use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;

const TEST_WAIT_BOUND: Duration = Duration::from_secs(15);

fn python_exe() -> String {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build Python probe runtime");
    runtime.block_on(async {
        for candidate in ["python3", "python"] {
            let mut probe = tokio::process::Command::new(candidate);
            probe
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let status = tokio::time::timeout(Duration::from_secs(5), probe.status())
                .await
                .expect("timed out probing Python interpreter");
            if status.is_ok_and(|status| status.success()) {
                return candidate.to_string();
            }
        }
        panic!("python3 or python is required for workflow_check tests");
    })
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("failed-jobs")
}

fn run_sxmc_bounded(args: &[&str], cwd: Option<&Path>, home: &Path) -> Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    rt.block_on(async {
        let mut cmd = tokio::process::Command::new(cargo_bin("sxmc"));
        cmd.args(args)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().expect("spawn sxmc");
        let mut stdout_pipe = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");
        let wait_fut = async {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let (out_res, err_res, status_res) = tokio::join!(
                stdout_pipe.read_to_end(&mut stdout),
                stderr_pipe.read_to_end(&mut stderr),
                child.wait() // lessons: allow L21 wait_fut is bounded by tokio::time::timeout(TEST_WAIT_BOUND, ...)
            );
            out_res.expect("read stdout");
            err_res.expect("read stderr");
            let status = status_res.expect("wait sxmc child");
            Output {
                status,
                stdout,
                stderr,
            }
        };

        match tokio::time::timeout(TEST_WAIT_BOUND, wait_fut).await {
            Ok(output) => output,
            Err(_) => {
                let cleanup_note = match child.start_kill() {
                    Ok(()) => {
                        let reap_fut = child.wait(); // lessons: allow L21 bounded by 2s tokio::time::timeout
                        match tokio::time::timeout(Duration::from_secs(2), reap_fut).await {
                            Ok(Ok(_)) => "killed and reaped".to_string(),
                            Ok(Err(err)) => format!("kill sent, wait failed: {err}"),
                            Err(_) => "kill sent, timed out waiting 2s to reap".to_string(),
                        }
                    }
                    Err(err) => format!("start_kill failed: {err}"),
                };
                panic!(
                    "sxmc {:?} exceeded test wait bound of {:?} ({})",
                    args, TEST_WAIT_BOUND, cleanup_note
                );
            }
        }
    })
}

fn parse_stdout_json(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&text).unwrap_or_else(|err| {
        panic!(
            "expected valid JSON on stdout (exit={:?}): {}\nstdout={}\nstderr={}",
            output.status.code(),
            err,
            text,
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn write_manifest(root: &Path, body: &str) {
    let dir = root.join(".sxmc");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("checks.toml"), body).unwrap();
}

#[test]
fn fixture_failed_jobs_v1_passes_human_and_json() {
    let home = TempDir::new().unwrap();
    let root = fixture_dir();
    let root_str = root.to_str().unwrap();

    let human = run_sxmc_bounded(&["check", "--root", root_str], None, home.path());
    assert_eq!(
        human.status.code(),
        Some(0),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&human.stdout),
        String::from_utf8_lossy(&human.stderr)
    );
    let human_text = String::from_utf8_lossy(&human.stdout);
    assert!(human_text.contains("PASS"), "human output: {human_text}");
    assert!(
        human_text.contains("failed-jobs-v1"),
        "human output: {human_text}"
    );

    let json_out = run_sxmc_bounded(
        &["check", "--root", root_str, "--format", "json"],
        None,
        home.path(),
    );
    assert_eq!(json_out.status.code(), Some(0));
    let report = parse_stdout_json(&json_out);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "passed");
    assert_eq!(report["exit_code"], 0);
    assert_eq!(report["summary"]["total"], 1);
    assert_eq!(report["summary"]["passed"], 1);
    assert_eq!(report["summary"]["failed"], 0);
    assert_eq!(report["summary"]["error"], 0);

    let check = &report["checks"][0];
    assert_eq!(check["name"], "failed-jobs-v1");
    assert_eq!(check["status"], "passed");
    assert_eq!(check["actual"]["exit_code"], 0);
    assert_eq!(check["actual"]["json_parsed"], true);
    assert_eq!(
        check["actual"]["json_values"]["/failed_job_ids"],
        serde_json::json!(["job-17"])
    );
    assert_eq!(check["actual"]["json_values"]["/failed_count"], 1);
    assert_eq!(check["actual"]["json_types"]["/last_error"], "null");
    assert_eq!(check["actual"]["json_values"]["/meta~1schema"], "v1");
    assert_eq!(
        check["actual"]["json_values"]["/meta~0source"],
        "fake_jobs_cli"
    );
}

#[test]
fn fixture_failed_jobs_extra_fields_pass() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();
    let script = fixture_dir().join("list_failed_jobs.py");

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "failed-jobs-extra"
argv = [{py:?}, {script:?}, "--mode", "v1-extra", "--extra-fields"]
expect_exit = 0

[checks.json_types]
"/failed_count" = "integer"
"/failed_job_ids" = "array"
"/last_error" = "null"

[checks.json_equals]
"/failed_count" = 1
"/failed_job_ids" = ["job-17"]
"#,
        py = py,
        script = script.to_string_lossy()
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let report = parse_stdout_json(&out);
    assert_eq!(report["status"], "passed");
    assert_eq!(report["checks"][0]["name"], "failed-jobs-extra");
}

#[test]
fn fixture_failed_jobs_v2_schema_drift_catches_false_zero_exit_1() {
    let home = TempDir::new().unwrap();
    let root = fixture_dir();
    let drift_manifest = root.join("checks-v2-drift.toml");

    let json_out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            root.to_str().unwrap(),
            "--manifest",
            drift_manifest.to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        home.path(),
    );
    assert_eq!(
        json_out.status.code(),
        Some(1),
        "stdout={}",
        String::from_utf8_lossy(&json_out.stdout)
    );
    let report = parse_stdout_json(&json_out);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["exit_code"], 1);
    assert_eq!(report["summary"]["failed"], 1);
    assert_eq!(report["summary"]["error"], 0);

    let check = &report["checks"][0];
    assert_eq!(check["name"], "failed-jobs-v2-drift");
    assert_eq!(check["status"], "failed");
    // Script falsely exited 0 with empty results.
    assert_eq!(check["actual"]["exit_code"], 0);
    assert_eq!(check["actual"]["json_values"]["/failed_count"], 0);
    assert_eq!(
        check["actual"]["json_values"]["/failed_job_ids"],
        serde_json::json!([])
    );
    assert_eq!(check["expected"]["json_equals"]["/failed_count"], 1);
    assert_eq!(
        check["expected"]["json_equals"]["/failed_job_ids"],
        serde_json::json!(["job-17"])
    );
    let reason = check["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("/failed_count") || reason.contains("/failed_job_ids"),
        "expected useful diagnostic in reason: {reason}"
    );

    let human_out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            root.to_str().unwrap(),
            "--manifest",
            drift_manifest.to_str().unwrap(),
        ],
        None,
        home.path(),
    );
    assert_eq!(human_out.status.code(), Some(1));
    let human_text = String::from_utf8_lossy(&human_out.stdout);
    assert!(human_text.contains("FAIL"), "human output: {human_text}");
    assert!(
        human_text.contains("expected") && human_text.contains("actual"),
        "human output missing expected/actual: {human_text}"
    );
}

#[test]
fn check_defaults_root_to_cwd_and_runs_in_project_root() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();
    fs::write(tmp.path().join("marker.txt"), "ok").unwrap();

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "cwd-check"
argv = [{py:?}, "-c", "import json, os; print(json.dumps({{'cwd': os.path.basename(os.getcwd()), 'has_marker': os.path.isfile('marker.txt')}}))"]
expect_exit = 0

[checks.json_equals]
"/has_marker" = true
"#,
        py = py
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(&["check", "--format", "json"], Some(tmp.path()), tmp.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let report = parse_stdout_json(&out);
    assert_eq!(report["status"], "passed");
    assert_eq!(
        report["checks"][0]["actual"]["json_values"]["/has_marker"],
        true
    );
}

#[test]
fn check_distinguishes_missing_vs_null_and_pointer_escapes() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "null-and-escapes"
argv = [{py:?}, "-c", "import json; print(json.dumps({{'present_null': None, 'a/b': {{'m~n': [10, 20]}}, '': 'empty-key'}}))"]
expect_exit = 0

[checks.json_types]
"" = "object"
"/present_null" = "null"
"/a~1b/m~0n/1" = "integer"
"/" = "string"

[checks.json_equals]
"/a~1b/m~0n/1" = 20
"/" = "empty-key"

[[checks]]
name = "missing-vs-null"
argv = [{py:?}, "-c", "import json; print(json.dumps({{'present_null': None}}))"]
expect_exit = 0

[checks.json_types]
"/missing_field" = "null"
"#,
        py = py
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    let report = parse_stdout_json(&out);
    assert_eq!(report["checks"][0]["status"], "passed");
    assert_eq!(report["checks"][1]["status"], "failed");
    assert_eq!(
        report["checks"][1]["actual"]["json_types"]["/missing_field"],
        "missing"
    );
}

#[test]
fn check_child_nonzero_and_invalid_json_exit_1() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();
    let cli = fixture_dir().join("fake_jobs_cli.py");

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "expected-nonzero-ok"
argv = [{py:?}, {cli:?}, "--mode", "nonzero"]
expect_exit = 3

[[checks]]
name = "unexpected-nonzero"
argv = [{py:?}, {cli:?}, "--mode", "nonzero"]
expect_exit = 0

[[checks]]
name = "invalid-json-output"
argv = [{py:?}, "-c", "print('not-valid-json')"]
expect_exit = 0

[checks.json_equals]
"/ok" = true
"#,
        py = py,
        cli = cli.to_string_lossy()
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    let report = parse_stdout_json(&out);
    assert_eq!(report["summary"]["passed"], 1);
    assert_eq!(report["summary"]["failed"], 2);
    assert_eq!(report["summary"]["error"], 0);
    assert_eq!(report["checks"][0]["status"], "passed");
    assert_eq!(report["checks"][1]["status"], "failed");
    assert_eq!(report["checks"][1]["actual"]["exit_code"], 3);
    assert_eq!(report["checks"][2]["status"], "failed");
    assert_eq!(report["checks"][2]["actual"]["json_parsed"], false);
}

#[test]
fn check_missing_executable_exits_2_and_wins_over_exit_1() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "expectation-mismatch"
argv = [{py:?}, "-c", "print('{{\"count\": 0}}')"]
expect_exit = 0

[checks.json_equals]
"/count" = 1

[[checks]]
name = "missing-exec"
argv = ["sxmc-nonexistent-binary-for-check-test"]
expect_exit = 0
"#,
        py = py
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(2));
    let report = parse_stdout_json(&out);
    assert_eq!(report["status"], "error");
    assert_eq!(report["exit_code"], 2);
    assert_eq!(report["summary"]["failed"], 1);
    assert_eq!(report["summary"]["error"], 1);
    assert_eq!(report["checks"][0]["status"], "failed");
    assert_eq!(report["checks"][1]["status"], "error");
}

#[test]
fn check_preflight_validation_rejects_before_execution() {
    let py = python_exe();
    let invalid_manifests = vec![
        ("wrong-version", "version = 2\n[[checks]]\nname = \"a\"\nargv = [\"echo\"]\n".to_string()),
        ("empty-checks", "version = 1\nchecks = []\n".to_string()),
        ("empty-name", format!("version = 1\n[[checks]]\nname = \"\"\nargv = [{py:?}, \"-c\", \"open('ran.txt','w').write('1')\"]\n")),
        ("whitespace-name", format!("version = 1\n[[checks]]\nname = \"bad name\"\nargv = [{py:?}, \"-c\", \"open('ran.txt','w').write('1')\"]\n")),
        ("duplicate-names", format!("version = 1\n[[checks]]\nname = \"dup\"\nargv = [{py:?}, \"-c\", \"open('ran.txt','w').write('1')\"]\n[[checks]]\nname = \"dup\"\nargv = [{py:?}, \"-c\", \"print(1)\"]\n")),
        ("empty-argv", "version = 1\n[[checks]]\nname = \"empty-argv\"\nargv = []\n".to_string()),
        ("empty-argv0", "version = 1\n[[checks]]\nname = \"empty-exe\"\nargv = [\"\"]\n".to_string()),
        ("bad-exit-code", "version = 1\n[[checks]]\nname = \"bad-exit\"\nargv = [\"echo\"]\nexpect_exit = 256\n".to_string()),
        ("bad-pointer-no-slash", format!("version = 1\n[[checks]]\nname = \"bad-ptr\"\nargv = [{py:?}, \"-c\", \"open('ran.txt','w').write('1')\"]\n[checks.json_equals]\n\"foo\" = 1\n")),
        ("bad-pointer-escape", format!("version = 1\n[[checks]]\nname = \"bad-esc\"\nargv = [{py:?}, \"-c\", \"open('ran.txt','w').write('1')\"]\n[checks.json_types]\n\"/a~2b\" = \"string\"\n")),
        ("bad-pointer-trailing-tilde", format!("version = 1\n[[checks]]\nname = \"bad-tilde\"\nargv = [{py:?}, \"-c\", \"open('ran.txt','w').write('1')\"]\n[checks.json_types]\n\"/a~\" = \"string\"\n")),
        ("unknown-json-type", format!("version = 1\n[[checks]]\nname = \"bad-type\"\nargv = [{py:?}, \"-c\", \"open('ran.txt','w').write('1')\"]\n[checks.json_types]\n\"/a\" = \"list\"\n")),
        ("unknown-toml-field", "version = 1\n[[checks]]\nname = \"typo\"\nargv = [\"echo\"]\nexepct_exit = 0\n".to_string()),
        ("invalid-toml", "version = [broken\n".to_string()),
    ];

    for (label, manifest_text) in invalid_manifests {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), &manifest_text);
        let out = run_sxmc_bounded(
            &[
                "check",
                "--root",
                tmp.path().to_str().unwrap(),
                "--format",
                "json",
            ],
            None,
            tmp.path(),
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "case '{label}' did not exit 2; stdout={}",
            String::from_utf8_lossy(&out.stdout)
        );
        let report = parse_stdout_json(&out);
        assert_eq!(report["status"], "error", "case '{label}'");
        assert_eq!(report["exit_code"], 2, "case '{label}'");
        assert!(
            report["error"]
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "case '{label}' missing error message"
        );
        assert!(
            !tmp.path().join("ran.txt").exists(),
            "case '{label}' executed command despite invalid manifest!"
        );
    }

    // Missing manifest and invalid CLI knobs (L7) must also fail preflight with exit 2.
    let tmp = TempDir::new().unwrap();
    let missing_manifest = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(missing_manifest.status.code(), Some(2));
    assert_eq!(parse_stdout_json(&missing_manifest)["exit_code"], 2);

    write_manifest(
        tmp.path(),
        &format!(
            "version = 1\n[[checks]]\nname = \"ok\"\nargv = [{py:?}, \"-c\", \"open('ran.txt','w').write('1')\"]\n",
            py = py
        ),
    );
    for bad_knob in [
        ["--timeout-seconds", "0"],
        ["--timeout-seconds", "3601"],
        ["--max-output-bytes", "0"],
        ["--max-output-bytes", "67108865"],
    ] {
        let out = run_sxmc_bounded(
            &[
                "check",
                "--root",
                tmp.path().to_str().unwrap(),
                bad_knob[0],
                bad_knob[1],
                "--format",
                "json",
            ],
            None,
            tmp.path(),
        );
        assert_eq!(out.status.code(), Some(2), "knob {:?}", bad_knob);
        assert_eq!(parse_stdout_json(&out)["exit_code"], 2);
        assert!(!tmp.path().join("ran.txt").exists());
    }

    let nonexistent_root = tmp.path().join("does-not-exist");
    let bad_root = run_sxmc_bounded(
        &[
            "check",
            "--root",
            nonexistent_root.to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(bad_root.status.code(), Some(2));
    assert_eq!(parse_stdout_json(&bad_root)["exit_code"], 2);
}

#[test]
fn check_number_vs_integer_types_and_array_index_bounds() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "types-and-arrays"
argv = [{py:?}, "-c", "import json; print(json.dumps({{'whole': 5, 'frac': 1.5, 'items': ['a']}}))"]
expect_exit = 0

[checks.json_types]
"/whole" = "number"
"/frac" = "integer"
"/items/01" = "string"
"/items/2" = "string"
"#,
        py = py
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    let report = parse_stdout_json(&out);
    assert_eq!(
        report["checks"][0]["actual"]["json_types"]["/whole"],
        "integer"
    );
    assert_eq!(
        report["checks"][0]["actual"]["json_types"]["/frac"],
        "number"
    );
    assert_eq!(
        report["checks"][0]["actual"]["json_types"]["/items/01"],
        "missing"
    );
    assert_eq!(
        report["checks"][0]["actual"]["json_types"]["/items/2"],
        "missing"
    );
    let mismatches = report["checks"][0]["mismatches"].as_array().unwrap();
    // /whole matched "number"; /frac, /items/01, /items/2 failed.
    assert_eq!(mismatches.len(), 3);
}

#[test]
fn check_no_shell_reinterpretation() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();
    let literal_arg = "hello; touch should_not_exist && echo $HOME";

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "literal-argv"
argv = [{py:?}, "-c", "import json, sys; print(json.dumps({{'arg': sys.argv[1]}}))", {literal_arg:?}]
expect_exit = 0

[checks.json_equals]
"/arg" = {literal_arg:?}
"#,
        py = py,
        literal_arg = literal_arg
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(!tmp.path().join("should_not_exist").exists());
}

#[test]
fn check_cli_reports_runner_timeout() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "slow-child"
argv = [{py:?}, "-c", "import time; time.sleep(30)"]
expect_exit = 0
"#,
        py = py
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--timeout-seconds",
            "1",
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(2));
    let report = parse_stdout_json(&out);
    assert_eq!(report["status"], "error");
    assert_eq!(
        report["checks"].as_array().map(|a| a.len()),
        Some(1),
        "unexpected preflight error in report: {report}"
    );
    assert_eq!(report["checks"][0]["status"], "error");
    let reason = report["checks"][0]["reason"].as_str().unwrap_or("");
    assert_eq!(reason, "command timed out after 1s");
}

#[test]
fn check_output_limits_bound_stdout_and_stderr_and_terminate_child() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "flood-stdout"
argv = [{py:?}, "-c", "import sys; sys.stdout.write('x' * 262144); sys.stdout.flush()"]
expect_exit = 0

[[checks]]
name = "flood-stderr"
argv = [{py:?}, "-c", "import sys; sys.stderr.write('e' * 262144); sys.stderr.flush()"]
expect_exit = 0
"#,
        py = py
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--max-output-bytes",
            "4096",
            "--timeout-seconds",
            "5",
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(2));
    let report = parse_stdout_json(&out);
    assert_eq!(report["summary"]["error"], 2);
    assert_eq!(report["checks"][0]["status"], "error");
    assert!(report["checks"][0]["reason"]
        .as_str()
        .unwrap_or("")
        .contains("stdout"));
    assert_eq!(report["checks"][1]["status"], "error");
    assert!(report["checks"][1]["reason"]
        .as_str()
        .unwrap_or("")
        .contains("stderr"));
    // Avoid logging full output in the report.
    let raw_report = String::from_utf8_lossy(&out.stdout);
    assert!(
        raw_report.len() < 8192,
        "report unexpectedly large: {}",
        raw_report.len()
    );
}

#[test]
fn check_rejects_duplicate_json_object_keys() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();

    let manifest = format!(
        r#"version = 1

[[checks]]
name = "dup-top-level"
argv = [{py:?}, "-c", "print('{{\"failed_count\":0,\"failed_count\":1}}')"]
expect_exit = 0

[checks.json_equals]
"/failed_count" = 1

[[checks]]
name = "dup-nested"
argv = [{py:?}, "-c", "print('{{\"meta\":{{\"a\":1,\"a\":2}}}}')"]
expect_exit = 0

[checks.json_types]
"/meta/a" = "integer"
"#,
        py = py
    );
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    let report = parse_stdout_json(&out);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["summary"]["passed"], 0);
    assert_eq!(report["summary"]["failed"], 2);
    assert_eq!(report["checks"][0]["actual"]["json_parsed"], false);
    assert!(report["checks"][0]["reason"]
        .as_str()
        .unwrap_or("")
        .contains("duplicate"));
    assert_eq!(report["checks"][1]["actual"]["json_parsed"], false);
    assert!(report["checks"][1]["reason"]
        .as_str()
        .unwrap_or("")
        .contains("duplicate"));
}

#[test]
fn check_report_marks_dropped_detail_at_exact_budget_boundary() {
    let (out, report) = run_long_pointer_check(&"a".repeat(128 * 1024 - 5));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(report["checks"][0]["status"], "failed");
    assert_eq!(report["truncated"], true);
}

#[test]
fn check_report_budgets_escaped_pointer_bytes() {
    let (out, report) = run_long_pointer_check(&"\\u0001".repeat(27_000));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(report["checks"][0]["status"], "failed");
    assert_eq!(report["truncated"], true);
    assert!(
        out.stdout.len() <= 128 * 1024 + 1024,
        "escaped pointer made report {} bytes",
        out.stdout.len()
    );
}

#[test]
fn check_report_budgets_preflight_error_bytes() {
    let tmp = TempDir::new().unwrap();
    let invalid_pointer = "\\u0001".repeat(27_000);
    write_manifest(
        tmp.path(),
        &format!(
            r#"version = 1
[[checks]]
name = "invalid-pointer"
argv = ["unused"]
[checks.json_equals]
"{invalid_pointer}" = 2
"#
        ),
    );
    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.len() <= 128 * 1024 + 1024,
        "preflight error made report {} bytes",
        out.stdout.len()
    );
    let report = parse_stdout_json(&out);
    assert_eq!(report["status"], "error");
    assert_eq!(report["truncated"], true);
    assert!(report["error"]
        .as_str()
        .unwrap()
        .starts_with("invalid manifest:"));
}

fn run_long_pointer_check(toml_key: &str) -> (Output, Value) {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();
    fs::write(
        tmp.path().join("output.json"),
        format!("{{\"{toml_key}\":1}}"),
    )
    .unwrap();
    write_manifest(
        tmp.path(),
        &format!(
            r#"version = 1
[[checks]]
name = "long-pointer"
argv = [{py:?}, "-c", "print(open('output.json').read())"]
[checks.json_equals]
"/{toml_key}" = 2
"#
        ),
    );
    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    let report = parse_stdout_json(&out);
    (out, report)
}

#[test]
fn check_report_previews_oversized_nested_root_values() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();
    let prefix = "v".repeat(24_000);
    write_manifest(
        tmp.path(),
        &format!(
            r#"version = 1
[[checks]]
name = "nested-root"
argv = [{py:?}, "-c", "import json; print(json.dumps({{'outer': {{'items': ['v' * 24000 + 'y']}}}}))"]
[checks.json_equals]
"" = {{ outer = {{ items = ["{prefix}x"] }} }}
"#
        ),
    );
    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.len() < 4096);
    let report = parse_stdout_json(&out);
    assert_eq!(report["truncated"], true);
    let check = &report["checks"][0];
    assert_eq!(check["status"], "failed");
    let marker = &check["expected"]["json_equals"][""];
    for value in [
        marker,
        &check["actual"]["json_values"][""],
        &check["mismatches"][0]["expected"],
        &check["mismatches"][0]["actual"],
    ] {
        assert_eq!(value, marker);
        assert_eq!(value["_truncated"], true);
        assert!(value["preview"]
            .as_str()
            .unwrap()
            .starts_with(r#"{"outer":{"items":["v"#));
    }
}

#[test]
fn check_suite_report_budget_bounds_large_outputs() {
    let tmp = TempDir::new().unwrap();
    let py = python_exe();

    // Exhaust the budget before a passing check and a check whose last assertion alone fails.
    let mut manifest = String::from("version = 1\n\n");
    for idx in 0..40 {
        manifest.push_str(&format!(
            r#"[[checks]]
name = "budget-{idx:02}"
argv = [{py:?}, "-c", "import json; print(json.dumps({{f'k{{i:02}}': ('v' * 140) + f'-{{i}}' for i in range(25)}}))"]
expect_exit = 0

[checks.json_equals]
"#,
            idx = idx,
            py = py
        ));
        for k in 0..25 {
            if idx < 38 || (idx == 39 && k == 24) {
                manifest.push_str(&format!("\"/k{k:02}\" = \"wrong-{k}\"\n"));
            } else {
                manifest.push_str(&format!("\"/k{k:02}\" = \"{}-{k}\"\n", "v".repeat(140)));
            }
        }
        manifest.push('\n');
    }
    write_manifest(tmp.path(), &manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    let report = parse_stdout_json(&out);
    assert_eq!(
        report["truncated"], true,
        "expected report.truncated == true"
    );
    assert_eq!(report["status"], "failed");
    assert_eq!(report["summary"]["total"], 40);
    assert_eq!(report["summary"]["failed"], 39);
    assert_eq!(report["summary"]["passed"], 1);
    let checks = report["checks"].as_array().unwrap();
    assert_eq!(checks[38]["name"], "budget-38");
    assert_eq!(checks[38]["status"], "passed");
    // Final check after budget exhaustion must still have been evaluated as failed.
    assert_eq!(report["checks"][39]["name"], "budget-39");
    assert_eq!(report["checks"][39]["status"], "failed");
    // Once exhausted, later checks must not append per-assertion markers or reasons.
    assert_eq!(
        report["checks"][39]["mismatches"]
            .as_array()
            .map(|m| m.len()),
        Some(0),
        "expected no retained mismatch markers after suite budget exhaustion"
    );
    assert_eq!(report["checks"][39]["reason"], "");
    assert_eq!(
        report["checks"][39]["actual"]["json_values"]
            .as_object()
            .map(|o| o.len()),
        Some(0)
    );

    let mut structure = report;
    for item in structure["checks"].as_array_mut().unwrap() {
        item["expected"]["json_types"] = serde_json::json!({});
        item["expected"]["json_equals"] = serde_json::json!({});
        item["actual"]["json_types"] = serde_json::json!({});
        item["actual"]["json_values"] = serde_json::json!({});
        item["actual"]["missing_pointers"] = serde_json::json!([]);
        item["mismatches"] = serde_json::json!([]);
        item["reason"] = serde_json::json!("");
    }
    let report_bytes = out.stdout.trim_ascii_end().len();
    let detail_bytes = report_bytes - serde_json::to_vec(&structure).unwrap().len();
    assert!(
        detail_bytes <= 128 * 1024,
        "retained optional diagnostics exceeded suite cap: {detail_bytes} bytes"
    );
    assert!(
        report_bytes <= 150 * 1024,
        "report was {report_bytes} bytes"
    );
}

#[cfg(windows)]
#[test]
fn check_rejects_windows_batch_entrypoints_on_windows() {
    let py = python_exe();
    let batch_cases = [
        "run.bat",
        "RUN.CMD",
        "./scripts/tool.Bat",
        r"C:\tools\job.cmd",
    ];
    for exe in batch_cases {
        let tmp = TempDir::new().unwrap();
        let manifest = format!(
            r#"version = 1

[[checks]]
name = "batch-entry"
argv = [{exe:?}, "--help"]
expect_exit = 0

[[checks]]
name = "should-not-run"
argv = [{py:?}, "-c", "open('ran.txt', 'w').write('1')"]
expect_exit = 0
"#,
            exe = exe,
            py = py
        );
        write_manifest(tmp.path(), &manifest);

        let out = run_sxmc_bounded(
            &[
                "check",
                "--root",
                tmp.path().to_str().unwrap(),
                "--format",
                "json",
            ],
            None,
            tmp.path(),
        );
        assert_eq!(out.status.code(), Some(2), "exe={exe}");
        let report = parse_stdout_json(&out);
        assert_eq!(report["status"], "error", "exe={exe}");
        assert!(
            report["error"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase()
                .contains("batch"),
            "unexpected error for {exe}: {}",
            report["error"]
        );
        assert!(!tmp.path().join("ran.txt").exists(), "exe={exe}");
    }
}

#[cfg(unix)]
#[test]
fn check_allows_native_unix_executable_with_cmd_suffix() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let py = python_exe();
    let script_path = tmp.path().join("tool.cmd");
    let body = format!(
        "#!{py}\nimport json, sys\nprint(json.dumps({{'arg': sys.argv[1]}}))\n",
        py = if Path::new(&py).is_absolute() {
            py.clone()
        } else {
            format!("/usr/bin/env {py}")
        }
    );
    fs::write(&script_path, body).unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    let manifest = r#"version = 1

[[checks]]
name = "unix-cmd-suffix"
argv = ["./tool.cmd", "hello; literal"]
expect_exit = 0

[checks.json_equals]
"/arg" = "hello; literal"
"#;
    write_manifest(tmp.path(), manifest);

    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let report = parse_stdout_json(&out);
    assert_eq!(report["status"], "passed");
    assert_eq!(
        report["checks"][0]["actual"]["json_values"]["/arg"],
        "hello; literal"
    );
}

#[test]
fn check_rejects_float_equality_assertions_at_any_nesting() {
    let py = python_exe();
    let float_manifests = [
        (
            "top-level-float",
            r#"version = 1
[[checks]]
name = "f1"
argv = ["echo"]
[checks.json_equals]
"/score" = 1.0
"#,
        ),
        (
            "nested-array-float",
            r#"version = 1
[[checks]]
name = "f2"
argv = ["echo"]
[checks.json_equals]
"/items" = [1, 2.5]
"#,
        ),
        (
            "nested-table-float",
            r#"version = 1
[[checks]]
name = "f3"
argv = ["echo"]
[checks.json_equals]
"/meta" = { ratio = 0.25 }
"#,
        ),
    ];

    for (label, manifest) in float_manifests {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), manifest);
        let out = run_sxmc_bounded(
            &[
                "check",
                "--root",
                tmp.path().to_str().unwrap(),
                "--format",
                "json",
            ],
            None,
            tmp.path(),
        );
        assert_eq!(out.status.code(), Some(2), "case={label}");
        let report = parse_stdout_json(&out);
        assert_eq!(report["status"], "error", "case={label}");
        assert!(
            report["error"]
                .as_str()
                .unwrap_or("")
                .contains("floating-point"),
            "case={label} error={}",
            report["error"]
        );
    }

    // Integer equality still rejects float output like 1.0, while json_types="number" accepts decimals.
    let tmp = TempDir::new().unwrap();
    write_manifest(
        tmp.path(),
        &format!(
            r#"version = 1

[[checks]]
name = "decimal-type-ok"
argv = [{py:?}, "-c", "print('{{\"ratio\": 1.5, \"count\": 1}}')"]
expect_exit = 0

[checks.json_types]
"/ratio" = "number"
"/count" = "integer"

[checks.json_equals]
"/count" = 1

[[checks]]
name = "float-vs-int-mismatch"
argv = [{py:?}, "-c", "print('{{\"count\": 1.0}}')"]
expect_exit = 0

[checks.json_equals]
"/count" = 1
"#,
            py = py
        ),
    );
    let out = run_sxmc_bounded(
        &[
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
        ],
        None,
        tmp.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    let report = parse_stdout_json(&out);
    assert_eq!(report["checks"][0]["status"], "passed");
    assert_eq!(report["checks"][1]["status"], "failed");
}

#[test]
fn check_clap_invalid_knobs_emit_stderr_before_json() {
    let tmp = TempDir::new().unwrap();
    write_manifest(
        tmp.path(),
        &format!(
            "version = 1\n[[checks]]\nname = \"ok\"\nargv = [{py:?}, \"-c\", \"print('ok')\"]\n",
            py = python_exe()
        ),
    );

    for args in [
        vec![
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--timeout-seconds",
            "invalid",
            "--format",
            "json",
        ],
        vec![
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--timeout-seconds",
            "-1",
            "--format",
            "json",
        ],
        vec![
            "check",
            "--root",
            tmp.path().to_str().unwrap(),
            "--max-output-bytes",
            "invalid",
            "--format",
            "json",
        ],
    ] {
        let out = run_sxmc_bounded(&args, None, tmp.path());
        assert_eq!(out.status.code(), Some(2), "args={:?}", args);
        assert!(
            out.stdout.is_empty(),
            "expected empty stdout on Clap error for {:?}, got: {}",
            args,
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            !out.stderr.is_empty(),
            "expected Clap diagnostic on stderr for {:?}",
            args
        );
    }
}
