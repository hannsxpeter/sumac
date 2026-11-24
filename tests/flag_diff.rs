use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn sxmc(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("sxmc").unwrap();
    cmd.timeout(Duration::from_secs(30));
    cmd.env("HOME", home);
    cmd.env("USERPROFILE", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
    cmd.env("APPDATA", home.join("AppData").join("Roaming"));
    cmd.env("LOCALAPPDATA", home.join("AppData").join("Local"));
    cmd
}

#[cfg(unix)]
fn write_opsctl_fixture(dir: &Path, version: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = dir.join("opsctl");
    let body = match version {
        "before" => {
            r#"#!/bin/sh
case "$*" in
  *"team repo --help"*)
    cat <<'EOF'
opsctl team repo - Manage team repository targets

Usage:
  opsctl team repo [--repo <NAME>] [OPTIONS]

Options:
      --repo <NAME>  Target repository
      --json         Emit JSON
EOF
    ;;
  *"team --help"*)
    cat <<'EOF'
opsctl team - Team commands

Usage:
  opsctl team <COMMAND>

Commands:
  repo  Manage team repository targets
EOF
    ;;
  *"repo --help"*)
    cat <<'EOF'
opsctl repo - Manage top-level repository targets

Usage:
  opsctl repo [--repo <NAME>] [OPTIONS]

Options:
      --repo <NAME>  Target repository
      --json         Emit JSON
EOF
    ;;
  *)
    cat <<'EOF'
opsctl - Operations control tool

Usage:
  opsctl [--repo <NAME>] --token <TOKEN> [OPTIONS] <COMMAND>

Commands:
  repo  Manage top-level repository targets
  team  Team commands

Options:
      --repo <NAME>    Target repository
      --token <TOKEN>  API token
      --json           Emit JSON
EOF
    ;;
esac
"#
        }
        "after" => {
            r#"#!/bin/sh
case "$*" in
  *"team repo --help"*)
    cat <<'EOF'
opsctl team repo - Manage team repository targets

Usage:
  opsctl team repo --repo <NAME> [OPTIONS]

Options:
      --repo <NAME>  Target repository
      --dry-run      Preview changes
      --json         Emit JSON
EOF
    ;;
  *"team --help"*)
    cat <<'EOF'
opsctl team - Team commands

Usage:
  opsctl team <COMMAND>

Commands:
  repo  Manage team repository targets
EOF
    ;;
  *"repo --help"*)
    cat <<'EOF'
opsctl repo - Manage top-level repository targets

Usage:
  opsctl repo [--repo <NAME>] [OPTIONS]

Options:
      --repo <NAME>  Target repository
      --json         Emit JSON
EOF
    ;;
  *)
    cat <<'EOF'
opsctl - Operations control tool

Usage:
  opsctl --repo <NAME> [--token <TOKEN>] [OPTIONS] <COMMAND>

Commands:
  repo  Manage top-level repository targets
  team  Team commands

Options:
      --repo <NAME>    Target repository
      --token <TOKEN>  API token
      --json           Emit JSON
EOF
    ;;
esac
"#
        }
        "ambiguous" => {
            r#"#!/bin/sh
case "$*" in
  *"team repo --help"*)
    cat <<'EOF'
opsctl team repo - Manage team repository targets

Usage:
  opsctl team repo (--repo <NAME> | --org <ORG>) [OPTIONS]

Options:
      --repo <NAME>  Target repository
      --org <ORG>    Target organization
      --json         Emit JSON
EOF
    ;;
  *"team --help"*)
    cat <<'EOF'
opsctl team - Team commands

Usage:
  opsctl team <COMMAND>

Commands:
  repo  Manage team repository targets
EOF
    ;;
  *"repo --help"*)
    cat <<'EOF'
opsctl repo - Manage top-level repository targets

Usage:
  opsctl repo [--repo <NAME>] [OPTIONS]

Options:
      --repo <NAME>  Target repository
      --json         Emit JSON
EOF
    ;;
  *)
    cat <<'EOF'
opsctl - Ambiguous CLI usage fixture

Usage:
  opsctl (--repo <NAME> | --org <ORG>) --env <ENV> [--json]

Commands:
  repo  Manage top-level repository targets
  team  Team commands

Options:
      --repo <NAME>  Repository name
      --org <ORG>    Organization name
      --env <ENV>    Target environment
      --json         Emit JSON
      --extra        Help-only flag without usage evidence
EOF
    ;;
esac
"#
        }
        "multiline-conflict" => {
            r#"#!/bin/sh
cat <<'EOF'
opsctl - Multi-line conflicting usage fixture

Usage:
  opsctl --repo <NAME> [--json]
  opsctl [--repo <NAME>] --all
EOF
"#
        }
        other => panic!("unexpected fixture version: {other}"),
    };

    fs::write(&script, body).unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    script
}

fn find_option<'a>(profile: &'a Value, name: &str) -> &'a Value {
    profile["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|opt| opt["name"] == name)
        .unwrap_or_else(|| panic!("expected option {name} in {}", profile["command"]))
}

fn find_nested<'a>(profile: &'a Value, path: &[&str]) -> &'a Value {
    let mut current = profile;
    for segment in path {
        let children = current["subcommand_profiles"].as_array().unwrap();
        current = children
            .iter()
            .find(|child| {
                let cmd = child["command"].as_str().unwrap_or_default();
                cmd == *segment || cmd.ends_with(&format!(" {segment}"))
            })
            .unwrap_or_else(|| panic!("missing nested profile for segment {segment}"));
    }
    current
}

#[test]
#[cfg(unix)]
fn test_inspect_distinguishes_optional_and_compulsory_flags_with_operands() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let script = write_opsctl_fixture(&bin_dir, "before");
    let before_out = sxmc(&home)
        .args([
            "inspect",
            "cli",
            script.to_str().unwrap(),
            "--depth",
            "2",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let before: Value = serde_json::from_slice(&before_out).unwrap();

    let root_repo = find_option(&before, "--repo");
    assert_eq!(root_repo["required"], json!(false));
    assert_eq!(root_repo["value_name"], json!("NAME"));
    assert_eq!(root_repo["required_source"], json!("usage_optional"));

    let root_token = find_option(&before, "--token");
    assert_eq!(root_token["required"], json!(true));
    assert_eq!(root_token["value_name"], json!("TOKEN"));
    assert_eq!(root_token["required_source"], json!("usage_compulsory"));

    let root_json = find_option(&before, "--json");
    assert_eq!(root_json.get("required"), None);
    assert_eq!(root_json.get("required_source"), None);

    let repo_profile = find_nested(&before, &["repo"]);
    assert_eq!(
        find_option(repo_profile, "--repo")["required"],
        json!(false)
    );

    let team_repo_profile = find_nested(&before, &["team", "repo"]);
    assert_eq!(
        find_option(team_repo_profile, "--repo")["required"],
        json!(false)
    );
}

#[test]
#[cfg(unix)]
fn test_inspect_respects_ambiguous_usage_forms() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let script = write_opsctl_fixture(&bin_dir, "ambiguous");
    let out = sxmc(&home)
        .args([
            "inspect",
            "cli",
            script.to_str().unwrap(),
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let profile: Value = serde_json::from_slice(&out).unwrap();

    assert_eq!(find_option(&profile, "--repo").get("required"), None);
    assert_eq!(
        find_option(&profile, "--repo")["required_source"],
        json!("usage_ambiguous")
    );
    assert_eq!(find_option(&profile, "--org").get("required"), None);
    assert_eq!(
        find_option(&profile, "--org")["required_source"],
        json!("usage_ambiguous")
    );
    assert_eq!(find_option(&profile, "--env")["required"], json!(true));
    assert_eq!(
        find_option(&profile, "--env")["required_source"],
        json!("usage_compulsory")
    );
    assert_eq!(find_option(&profile, "--json")["required"], json!(false));
    assert_eq!(
        find_option(&profile, "--json")["required_source"],
        json!("usage_optional")
    );
    assert_eq!(find_option(&profile, "--extra").get("required"), None);
    assert_eq!(
        find_option(&profile, "--extra").get("required_source"),
        None
    );

    let conflict_dir = temp.path().join("bin-conflict");
    fs::create_dir_all(&conflict_dir).unwrap();
    let conflict_script = write_opsctl_fixture(&conflict_dir, "multiline-conflict");
    let conflict_out = sxmc(&home)
        .args([
            "inspect",
            "cli",
            conflict_script.to_str().unwrap(),
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let conflict_profile: Value = serde_json::from_slice(&conflict_out).unwrap();
    assert_eq!(
        find_option(&conflict_profile, "--repo").get("required"),
        None
    );
    assert_eq!(
        find_option(&conflict_profile, "--repo")["required_source"],
        json!("usage_ambiguous")
    );
}

#[test]
#[cfg(unix)]
fn test_inspect_diff_end_to_end_flag_transitions_nested_paths_and_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let script = write_opsctl_fixture(&bin_dir, "before");
    let before_out = sxmc(&home)
        .args([
            "inspect",
            "cli",
            script.to_str().unwrap(),
            "--depth",
            "2",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let before_path = temp.path().join("before.json");
    fs::write(&before_path, &before_out).unwrap();

    // Stable control: diffing against identical live CLI has zero tracked changes and exits 0.
    let stable_out = sxmc(&home)
        .args([
            "inspect",
            "diff",
            script.to_str().unwrap(),
            "--before",
            before_path.to_str().unwrap(),
            "--depth",
            "2",
            "--exit-code",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stable: Value = serde_json::from_slice(&stable_out).unwrap();
    assert_eq!(stable["options_became_required"], json!([]));
    assert_eq!(stable["options_became_optional"], json!([]));
    assert_eq!(stable["nested_options_added"], json!([]));
    assert_eq!(stable["nested_options_removed"], json!([]));
    assert_eq!(stable["option_changes"], json!([]));

    // Advance fixture to "after" (distinct byte length invalidates executable fingerprint cache).
    let before_len = fs::metadata(&script).unwrap().len();
    write_opsctl_fixture(&bin_dir, "after");
    let after_len = fs::metadata(&script).unwrap().len();
    assert_ne!(before_len, after_len);

    let diff_out = sxmc(&home)
        .args([
            "inspect",
            "diff",
            script.to_str().unwrap(),
            "--before",
            before_path.to_str().unwrap(),
            "--depth",
            "2",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let diff: Value = serde_json::from_slice(&diff_out).unwrap();

    assert_eq!(diff["options_added"], json!([]));
    assert_eq!(diff["options_removed"], json!([]));
    assert_eq!(
        diff["options_became_required"],
        json!(["--repo", "team repo --repo"])
    );
    assert_eq!(diff["options_became_optional"], json!(["--token"]));
    assert_eq!(diff["nested_options_added"], json!(["team repo --dry-run"]));
    assert_eq!(diff["nested_options_removed"], json!([]));
    assert_eq!(
        diff["option_changes"],
        json!([
            {
                "command_path": [],
                "command": "opsctl",
                "name": "--repo",
                "change": "became_required",
                "before_required": false,
                "after_required": true,
                "before_required_source": "usage_optional",
                "after_required_source": "usage_compulsory"
            },
            {
                "command_path": [],
                "command": "opsctl",
                "name": "--token",
                "change": "became_optional",
                "before_required": true,
                "after_required": false,
                "before_required_source": "usage_compulsory",
                "after_required_source": "usage_optional"
            },
            {
                "command_path": ["team", "repo"],
                "command": "opsctl team repo",
                "name": "--dry-run",
                "change": "added",
                "before_required": null,
                "after_required": null,
                "before_required_source": null,
                "after_required_source": null
            },
            {
                "command_path": ["team", "repo"],
                "command": "opsctl team repo",
                "name": "--repo",
                "change": "became_required",
                "before_required": false,
                "after_required": true,
                "before_required_source": "usage_optional",
                "after_required_source": "usage_compulsory"
            }
        ])
    );

    // --exit-code must exit non-zero when semantic flag changes are present.
    sxmc(&home)
        .args([
            "inspect",
            "diff",
            script.to_str().unwrap(),
            "--before",
            before_path.to_str().unwrap(),
            "--depth",
            "2",
            "--exit-code",
        ])
        .assert()
        .failure();

    // TOON output includes both directions and nested changes.
    sxmc(&home)
        .args([
            "inspect",
            "diff",
            script.to_str().unwrap(),
            "--before",
            before_path.to_str().unwrap(),
            "--depth",
            "2",
            "--format",
            "toon",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("options_became_required:"))
        .stdout(predicate::str::contains("- --repo"))
        .stdout(predicate::str::contains("- team repo --repo"))
        .stdout(predicate::str::contains("options_became_optional:"))
        .stdout(predicate::str::contains("- --token"))
        .stdout(predicate::str::contains("nested_options_added:"))
        .stdout(predicate::str::contains("- team repo --dry-run"));

    // Markdown (human) output includes both directions and nested changes.
    sxmc(&home)
        .args([
            "inspect",
            "diff",
            script.to_str().unwrap(),
            "--before",
            before_path.to_str().unwrap(),
            "--depth",
            "2",
            "--format",
            "markdown",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("## Options became required"))
        .stdout(predicate::str::contains("- `--repo`"))
        .stdout(predicate::str::contains("- `team repo --repo`"))
        .stdout(predicate::str::contains("## Options became optional"))
        .stdout(predicate::str::contains("- `--token`"))
        .stdout(predicate::str::contains("## Added nested options"))
        .stdout(predicate::str::contains("- `team repo --dry-run`"));
}

#[test]
#[cfg(unix)]
fn test_legacy_profile_and_explicit_required_values_remain_usable() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let legacy_before = json!({
        "profile_schema": "sxmc_cli_surface_profile_v1",
        "command": "opsctl",
        "summary": "Legacy opsctl summary.",
        "source": {
            "kind": "cli",
            "identifier": "opsctl"
        },
        "subcommands": [],
        "options": [
            {
                "name": "--repo",
                "value_name": "NAME",
                "required": false,
                "summary": "Target repository",
                "confidence": "high"
            },
            {
                "name": "--token",
                "value_name": "TOKEN",
                "required": true,
                "summary": "API token",
                "confidence": "high"
            }
        ],
        "positionals": [],
        "examples": [],
        "auth": [],
        "environment": [],
        "output_behavior": {
            "stdout_style": "plain_text",
            "stderr_usage": "Unknown",
            "machine_friendly": false
        },
        "workflows": [],
        "confidence_notes": [],
        "provenance": {
            "generated_by": "sxmc",
            "generator_version": "0.9.0",
            "source_kind": "cli",
            "source_identifier": "opsctl",
            "profile_schema": "sxmc_cli_surface_profile_v1",
            "generation_depth": 0
        }
    });

    let mut legacy_after = legacy_before.clone();
    legacy_after["options"][0]["required"] = json!(true);
    legacy_after["options"][1]["required"] = json!(false);

    let before_path = temp.path().join("legacy-before.json");
    let after_path = temp.path().join("legacy-after.json");
    fs::write(
        &before_path,
        serde_json::to_string_pretty(&legacy_before).unwrap(),
    )
    .unwrap();
    fs::write(
        &after_path,
        serde_json::to_string_pretty(&legacy_after).unwrap(),
    )
    .unwrap();

    let diff_out = sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            before_path.to_str().unwrap(),
            "--after",
            after_path.to_str().unwrap(),
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let diff: Value = serde_json::from_slice(&diff_out).unwrap();

    assert_eq!(diff["options_became_required"], json!(["--repo"]));
    assert_eq!(diff["options_became_optional"], json!(["--token"]));
    assert_eq!(
        diff["option_changes"],
        json!([
            {
                "command_path": [],
                "command": "opsctl",
                "name": "--repo",
                "change": "became_required",
                "before_required": false,
                "after_required": true,
                "before_required_source": null,
                "after_required_source": null
            },
            {
                "command_path": [],
                "command": "opsctl",
                "name": "--token",
                "change": "became_optional",
                "before_required": true,
                "after_required": false,
                "before_required_source": null,
                "after_required_source": null
            }
        ])
    );

    sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            before_path.to_str().unwrap(),
            "--after",
            after_path.to_str().unwrap(),
            "--exit-code",
        ])
        .assert()
        .failure();
}

#[test]
#[cfg(unix)]
fn test_diff_does_not_misreport_uninspected_child_flags_when_depth_differs() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let script = write_opsctl_fixture(&bin_dir, "before");
    let shallow_out = sxmc(&home)
        .args([
            "inspect",
            "cli",
            script.to_str().unwrap(),
            "--depth",
            "0",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let deep_out = sxmc(&home)
        .args([
            "inspect",
            "cli",
            script.to_str().unwrap(),
            "--depth",
            "2",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let shallow_path = temp.path().join("shallow.json");
    let deep_path = temp.path().join("deep.json");
    fs::write(&shallow_path, &shallow_out).unwrap();
    fs::write(&deep_path, &deep_out).unwrap();

    let up_out = sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            shallow_path.to_str().unwrap(),
            "--after",
            deep_path.to_str().unwrap(),
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let up_diff: Value = serde_json::from_slice(&up_out).unwrap();
    assert_eq!(up_diff["nested_options_added"], json!([]));
    assert_eq!(up_diff["nested_options_removed"], json!([]));
    assert_eq!(up_diff["option_changes"], json!([]));

    let down_out = sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            deep_path.to_str().unwrap(),
            "--after",
            shallow_path.to_str().unwrap(),
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let down_diff: Value = serde_json::from_slice(&down_out).unwrap();
    assert_eq!(down_diff["nested_options_added"], json!([]));
    assert_eq!(down_diff["nested_options_removed"], json!([]));
    assert_eq!(down_diff["option_changes"], json!([]));
}

#[test]
#[cfg(unix)]
fn test_status_and_sync_detect_flag_requiredness_drift() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let project_root = temp.path().join("project");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();

    write_opsctl_fixture(&bin_dir, "before");
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "add",
            "opsctl",
            "--host",
            "claude-code",
            "--depth",
            "2",
            "--root",
            project_root.to_str().unwrap(),
            "--format",
            "json-pretty",
        ])
        .assert()
        .success();

    let clean_status_out = sxmc(&home)
        .env("PATH", &path_env)
        .current_dir(&project_root)
        .args(["status", "--format", "json-pretty"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let clean_status: Value = serde_json::from_slice(&clean_status_out).unwrap();
    assert_eq!(
        clean_status["saved_profiles"]["drift"]["changed_count"],
        json!(0)
    );
    assert_eq!(clean_status["sync_state"]["current_drift_count"], json!(0));

    sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "sync",
            "--root",
            project_root.to_str().unwrap(),
            "--check",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success();

    let before_len = fs::metadata(bin_dir.join("opsctl")).unwrap().len();
    write_opsctl_fixture(&bin_dir, "after");
    let after_len = fs::metadata(bin_dir.join("opsctl")).unwrap().len();
    assert_ne!(before_len, after_len);

    let drifted_status_out = sxmc(&home)
        .env("PATH", &path_env)
        .current_dir(&project_root)
        .args(["status", "--format", "json-pretty"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let drifted_status: Value = serde_json::from_slice(&drifted_status_out).unwrap();
    assert_eq!(
        drifted_status["saved_profiles"]["drift"]["changed_count"],
        json!(1)
    );
    assert_eq!(
        drifted_status["sync_state"]["current_drift_count"],
        json!(1)
    );
    assert_eq!(
        drifted_status["sync_state"]["commands_needing_sync"],
        json!(["opsctl"])
    );
    let drift_entry = &drifted_status["saved_profiles"]["drift"]["entries"][0];
    assert_eq!(drift_entry["changed"], json!(true));
    assert_eq!(
        drift_entry["diff"]["options_became_required"],
        json!(["--repo", "team repo --repo"])
    );
    assert_eq!(
        drift_entry["diff"]["options_became_optional"],
        json!(["--token"])
    );

    sxmc(&home)
        .env("PATH", &path_env)
        .current_dir(&project_root)
        .args(["status", "--human"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Changed profiles:"))
        .stdout(predicate::str::contains("opsctl"));

    sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "sync",
            "--root",
            project_root.to_str().unwrap(),
            "--check",
            "--format",
            "json-pretty",
        ])
        .assert()
        .failure();

    sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "sync",
            "--root",
            project_root.to_str().unwrap(),
            "--apply",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success();

    sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "sync",
            "--root",
            project_root.to_str().unwrap(),
            "--check",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success();
}

#[test]
#[cfg(unix)]
fn test_h3_unknown_and_omitted_required_do_not_trigger_definitive_drift() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let project_root = temp.path().join("project");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();

    // 1. known -> ambiguous must NOT report became_optional or drift in diff/status/sync.
    write_opsctl_fixture(&bin_dir, "after");
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "add",
            "opsctl",
            "--host",
            "claude-code",
            "--depth",
            "2",
            "--root",
            project_root.to_str().unwrap(),
            "--format",
            "json-pretty",
        ])
        .assert()
        .success();

    let saved_path = project_root
        .join(".sxmc")
        .join("ai")
        .join("profiles")
        .join("opsctl.json");
    let saved_before: Value =
        serde_json::from_str(&fs::read_to_string(&saved_path).unwrap()).unwrap();
    // In "after", --repo is compulsory at root and at team repo.
    assert_eq!(
        find_option(&saved_before, "--repo")["required"],
        json!(true)
    );

    let before_len = fs::metadata(bin_dir.join("opsctl")).unwrap().len();
    write_opsctl_fixture(&bin_dir, "ambiguous");
    let ambig_len = fs::metadata(bin_dir.join("opsctl")).unwrap().len();
    assert_ne!(before_len, ambig_len);

    // Inspecting ambiguous live CLI vs known-compulsory saved profile (with matching summary/other fields)
    // must report NO became_optional transition for --repo at root or nested team repo.
    let ambig_profile_out = sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "inspect",
            "cli",
            "opsctl",
            "--depth",
            "2",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let mut ambig_profile: Value = serde_json::from_slice(&ambig_profile_out).unwrap();
    // Assert live root and live nested `team repo` `--repo` both omit `required` and carry
    // `usage_ambiguous` before reshaping any test JSON.
    let live_root_repo = find_option(&ambig_profile, "--repo");
    assert_eq!(live_root_repo.get("required"), None);
    assert_eq!(live_root_repo["required_source"], json!("usage_ambiguous"));
    let live_team_repo = find_nested(&ambig_profile, &["team", "repo"]);
    let live_team_repo_flag = find_option(live_team_repo, "--repo");
    assert_eq!(live_team_repo_flag.get("required"), None);
    assert_eq!(
        live_team_repo_flag["required_source"],
        json!("usage_ambiguous")
    );

    // Keep only --repo at root and team repo to isolate known->ambiguous without summary/other noise.
    let mut known_control = saved_before.clone();
    known_control["summary"] = ambig_profile["summary"].clone();
    known_control["environment"] = ambig_profile["environment"].clone();
    known_control["options"] = json!([{
        "name": "--repo",
        "value_name": "NAME",
        "required": true,
        "required_source": "usage_compulsory",
        "confidence": "high"
    }]);
    known_control["subcommands"] = json!([
        { "name": "repo", "summary": "Manage top-level repository targets", "confidence": "high" },
        { "name": "team", "summary": "Team commands", "confidence": "high" }
    ]);
    let team_node = known_control["subcommand_profiles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|p| p["command"] == "opsctl team")
        .unwrap();
    let team_repo_node = team_node["subcommand_profiles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|p| p["command"] == "opsctl team repo")
        .unwrap();
    team_repo_node["options"] = json!([{
        "name": "--repo",
        "value_name": "NAME",
        "required": true,
        "required_source": "usage_compulsory",
        "confidence": "high"
    }]);

    ambig_profile["options"] = json!([{
        "name": "--repo",
        "value_name": "NAME",
        "required_source": "usage_ambiguous",
        "confidence": "high"
    }]);
    let ambig_team = ambig_profile["subcommand_profiles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|p| p["command"] == "opsctl team")
        .unwrap();
    let ambig_team_repo = ambig_team["subcommand_profiles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|p| p["command"] == "opsctl team repo")
        .unwrap();
    ambig_team_repo["options"] = json!([{
        "name": "--repo",
        "value_name": "NAME",
        "required_source": "usage_ambiguous",
        "confidence": "high"
    }]);

    let known_path = temp.path().join("known-before.json");
    let ambig_path = temp.path().join("ambig-after.json");
    fs::write(
        &known_path,
        serde_json::to_string_pretty(&known_control).unwrap(),
    )
    .unwrap();
    fs::write(
        &ambig_path,
        serde_json::to_string_pretty(&ambig_profile).unwrap(),
    )
    .unwrap();

    let ka_diff_out = sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            known_path.to_str().unwrap(),
            "--after",
            ambig_path.to_str().unwrap(),
            "--exit-code",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ka_diff: Value = serde_json::from_slice(&ka_diff_out).unwrap();
    assert_eq!(ka_diff["options_became_optional"], json!([]));
    assert_eq!(ka_diff["options_became_required"], json!([]));
    assert_eq!(ka_diff["option_changes"], json!([]));

    // TOON and Markdown renderers must agree (no became_required/became_optional sections).
    sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            known_path.to_str().unwrap(),
            "--after",
            ambig_path.to_str().unwrap(),
            "--format",
            "toon",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("options_became_optional:").not())
        .stdout(predicate::str::contains("options_became_required:").not());
    sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            known_path.to_str().unwrap(),
            "--after",
            ambig_path.to_str().unwrap(),
            "--format",
            "markdown",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Options became optional").not())
        .stdout(predicate::str::contains("Options became required").not());

    // Verify status and sync agree when the saved profile matches the live ambiguous CLI
    // except that saved had known required:true and live has usage_ambiguous.
    fs::write(
        &saved_path,
        serde_json::to_string_pretty(&{
            let mut live_aligned = serde_json::from_slice::<Value>(&ambig_profile_out).unwrap();
            // Make saved profile have known required:true for --repo at root & team repo while keeping everything else identical to live ambiguous.
            let mut root_updated = false;
            for opt in live_aligned["options"].as_array_mut().unwrap() {
                if opt["name"] == "--repo" {
                    assert_eq!(opt.get("required"), None);
                    assert_eq!(opt["required_source"], json!("usage_ambiguous"));
                    opt["required"] = json!(true);
                    opt["required_source"] = json!("usage_compulsory");
                    root_updated = true;
                }
            }
            assert!(root_updated);
            let mut nested_updated = false;
            for child in live_aligned["subcommand_profiles"].as_array_mut().unwrap() {
                if child["command"] == "opsctl team" {
                    for gchild in child["subcommand_profiles"].as_array_mut().unwrap() {
                        if gchild["command"] == "opsctl team repo" {
                            for opt in gchild["options"].as_array_mut().unwrap() {
                                if opt["name"] == "--repo" {
                                    assert_eq!(opt.get("required"), None);
                                    assert_eq!(opt["required_source"], json!("usage_ambiguous"));
                                    opt["required"] = json!(true);
                                    opt["required_source"] = json!("usage_compulsory");
                                    nested_updated = true;
                                }
                            }
                        }
                    }
                }
            }
            assert!(nested_updated);
            live_aligned
        })
        .unwrap(),
    )
    .unwrap();

    let status_out = sxmc(&home)
        .env("PATH", &path_env)
        .current_dir(&project_root)
        .args(["status", "--format", "json-pretty"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&status_out).unwrap();
    assert_eq!(status["saved_profiles"]["drift"]["changed_count"], json!(0));
    assert_eq!(status["sync_state"]["current_drift_count"], json!(0));

    sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "sync",
            "--root",
            project_root.to_str().unwrap(),
            "--check",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success();

    // 2. Omitted legacy `required` vs known compulsory must NOT report became_required,
    //    and migrate-profile roundtrip must NOT turn omitted into `required: false`.
    let mut omitted_legacy = known_control.clone();
    omitted_legacy["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("required");
    omitted_legacy["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("required_source");
    omitted_legacy["subcommand_profiles"][1]["subcommand_profiles"][0]["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("required");
    omitted_legacy["subcommand_profiles"][1]["subcommand_profiles"][0]["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("required_source");
    let omitted_path = temp.path().join("omitted-legacy.json");
    let migrated_path = temp.path().join("omitted-migrated.json");
    fs::write(
        &omitted_path,
        serde_json::to_string_pretty(&omitted_legacy).unwrap(),
    )
    .unwrap();

    sxmc(&home)
        .args([
            "inspect",
            "migrate-profile",
            omitted_path.to_str().unwrap(),
            "--output",
            migrated_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let migrated: Value =
        serde_json::from_str(&fs::read_to_string(&migrated_path).unwrap()).unwrap();
    assert_eq!(migrated["options"][0].get("required"), None);
    assert_eq!(
        migrated["subcommand_profiles"][1]["subcommand_profiles"][0]["options"][0].get("required"),
        None
    );

    let ok_diff_out = sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            migrated_path.to_str().unwrap(),
            "--after",
            known_path.to_str().unwrap(),
            "--exit-code",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ok_diff: Value = serde_json::from_slice(&ok_diff_out).unwrap();
    assert_eq!(ok_diff["options_became_required"], json!([]));
    assert_eq!(ok_diff["options_became_optional"], json!([]));
    assert_eq!(ok_diff["option_changes"], json!([]));

    // Verify status and sync agree for omitted -> known on a saved profile against live compulsory "after".
    let before_len_2 = fs::metadata(bin_dir.join("opsctl")).unwrap().len();
    write_opsctl_fixture(&bin_dir, "after");
    let after_len_2 = fs::metadata(bin_dir.join("opsctl")).unwrap().len();
    assert_ne!(before_len_2, after_len_2);
    let live_after_out = sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "inspect",
            "cli",
            "opsctl",
            "--depth",
            "2",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let mut omitted_live = serde_json::from_slice::<Value>(&live_after_out).unwrap();
    let mut omitted_root = false;
    for opt in omitted_live["options"].as_array_mut().unwrap() {
        if opt["name"] == "--repo" {
            omitted_root = true;
        }
        opt.as_object_mut().unwrap().remove("required");
        opt.as_object_mut().unwrap().remove("required_source");
    }
    assert!(omitted_root);
    let mut omitted_nested = false;
    for child in omitted_live["subcommand_profiles"].as_array_mut().unwrap() {
        if child["command"] == "opsctl team" {
            for gchild in child["subcommand_profiles"].as_array_mut().unwrap() {
                if gchild["command"] == "opsctl team repo" {
                    for opt in gchild["options"].as_array_mut().unwrap() {
                        if opt["name"] == "--repo" {
                            omitted_nested = true;
                        }
                        opt.as_object_mut().unwrap().remove("required");
                        opt.as_object_mut().unwrap().remove("required_source");
                    }
                }
            }
        }
    }
    assert!(omitted_nested);
    fs::write(
        &saved_path,
        serde_json::to_string_pretty(&omitted_live).unwrap(),
    )
    .unwrap();
    let omitted_status_out = sxmc(&home)
        .env("PATH", &path_env)
        .current_dir(&project_root)
        .args(["status", "--format", "json-pretty"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let omitted_status: Value = serde_json::from_slice(&omitted_status_out).unwrap();
    assert_eq!(
        omitted_status["saved_profiles"]["drift"]["changed_count"],
        json!(0)
    );
    sxmc(&home)
        .env("PATH", &path_env)
        .args([
            "sync",
            "--root",
            project_root.to_str().unwrap(),
            "--check",
            "--format",
            "json-pretty",
        ])
        .assert()
        .success();

    // 3. Explicit legacy `required: false` -> known compulsory MUST report became_required at both root and nested paths;
    //    and explicit legacy `required: true` -> known compulsory is an unchanged control.
    let mut explicit_false_legacy = known_control.clone();
    explicit_false_legacy["options"][0]["required"] = json!(false);
    explicit_false_legacy["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("required_source");
    explicit_false_legacy["subcommand_profiles"][1]["subcommand_profiles"][0]["options"][0]
        ["required"] = json!(false);
    explicit_false_legacy["subcommand_profiles"][1]["subcommand_profiles"][0]["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("required_source");
    let explicit_false_path = temp.path().join("explicit-false-legacy.json");
    fs::write(
        &explicit_false_path,
        serde_json::to_string_pretty(&explicit_false_legacy).unwrap(),
    )
    .unwrap();

    let ef_diff_out = sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            explicit_false_path.to_str().unwrap(),
            "--after",
            known_path.to_str().unwrap(),
            "--format",
            "json-pretty",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ef_diff: Value = serde_json::from_slice(&ef_diff_out).unwrap();
    assert_eq!(
        ef_diff["options_became_required"],
        json!(["--repo", "team repo --repo"])
    );
    sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            explicit_false_path.to_str().unwrap(),
            "--after",
            known_path.to_str().unwrap(),
            "--exit-code",
        ])
        .assert()
        .failure();

    let mut explicit_true_legacy = known_control.clone();
    explicit_true_legacy["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("required_source");
    explicit_true_legacy["subcommand_profiles"][1]["subcommand_profiles"][0]["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("required_source");
    let explicit_true_path = temp.path().join("explicit-true-legacy.json");
    fs::write(
        &explicit_true_path,
        serde_json::to_string_pretty(&explicit_true_legacy).unwrap(),
    )
    .unwrap();
    sxmc(&home)
        .args([
            "inspect",
            "diff",
            "--before",
            explicit_true_path.to_str().unwrap(),
            "--after",
            known_path.to_str().unwrap(),
            "--exit-code",
        ])
        .assert()
        .success();
}
