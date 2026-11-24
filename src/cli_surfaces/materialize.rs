use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::cli_surfaces::model::{
    host_profile_spec, AiClientProfile, ApplyStrategy, ArtifactAudience, ArtifactMode,
    CliSurfaceProfile, ConfigShape, GeneratedArtifact, WriteOutcome, WriteStatus, AI_HOST_SPECS,
};
use crate::cli_surfaces::render::{
    format_toml_key, render_agent_doc, render_ci_workflow, render_client_config, render_llms_txt,
    render_mcp_wrapper_readme, render_portable_agent_doc, render_skill_markdown, slugify,
};
use crate::error::{Result, SxmcError};
use crate::paths::InstallPaths;

pub fn generate_profile_artifact(
    profile: &CliSurfaceProfile,
    install_paths: &InstallPaths,
) -> Result<GeneratedArtifact> {
    let slug = slugify(&profile.command);
    let target_path = install_paths.profile_dir().join(format!("{slug}.json"));
    let content = serde_json::to_string_pretty(profile)?;
    Ok(GeneratedArtifact {
        label: "CLI profile".into(),
        target_path,
        content,
        apply_strategy: ApplyStrategy::DirectWrite,
        audience: ArtifactAudience::Shared,
        sidecar_scope: "profiles".into(),
    })
}

pub fn generate_agent_doc_artifact(
    profile: &CliSurfaceProfile,
    client: AiClientProfile,
    install_paths: &InstallPaths,
) -> GeneratedArtifact {
    let spec = host_profile_spec(client);
    let target_path = install_paths
        .host_doc_path(client)
        .unwrap_or_else(|| install_paths.portable_agent_doc_path());
    let content = render_agent_doc(profile, client);
    GeneratedArtifact {
        label: format!("{} agent doc", spec.label),
        target_path,
        content,
        apply_strategy: ApplyStrategy::ManagedMarkdownBlock,
        audience: ArtifactAudience::Client(client),
        sidecar_scope: spec.sidecar_scope.into(),
    }
}

pub fn generate_portable_agent_doc_artifact(
    profile: &CliSurfaceProfile,
    install_paths: &InstallPaths,
) -> GeneratedArtifact {
    GeneratedArtifact {
        label: "Portable agent doc".into(),
        target_path: install_paths.portable_agent_doc_path(),
        content: render_portable_agent_doc(profile),
        apply_strategy: ApplyStrategy::ManagedMarkdownBlock,
        audience: ArtifactAudience::Portable,
        sidecar_scope: "portable".into(),
    }
}

pub fn generate_host_native_agent_doc_artifacts(
    profile: &CliSurfaceProfile,
    install_paths: &InstallPaths,
) -> Vec<GeneratedArtifact> {
    AI_HOST_SPECS
        .iter()
        .filter(|spec| {
            spec.native_doc_target.is_some()
                && !matches!(
                    spec.client,
                    AiClientProfile::GenericStdioMcp | AiClientProfile::GenericHttpMcp
                )
        })
        .map(|spec| GeneratedArtifact {
            label: format!("{} agent doc", spec.label),
            target_path: install_paths
                .host_doc_path(spec.client)
                .expect("checked above"),
            content: render_agent_doc(profile, spec.client),
            apply_strategy: ApplyStrategy::ManagedMarkdownBlock,
            audience: ArtifactAudience::Client(spec.client),
            sidecar_scope: spec.sidecar_scope.into(),
        })
        .collect()
}

pub fn generate_full_coverage_init_artifacts(
    profile: &CliSurfaceProfile,
    install_paths: &InstallPaths,
    skills_path: &Path,
) -> Result<Vec<GeneratedArtifact>> {
    let mut artifacts = vec![generate_profile_artifact(profile, install_paths)?];
    artifacts.push(generate_portable_agent_doc_artifact(profile, install_paths));
    artifacts.extend(generate_host_native_agent_doc_artifacts(
        profile,
        install_paths,
    ));

    for spec in AI_HOST_SPECS {
        if let Some(artifact) =
            generate_client_config_artifact(profile, spec.client, install_paths, skills_path)
        {
            artifacts.push(artifact);
        }
    }

    Ok(artifacts)
}

pub fn generate_client_config_artifact(
    profile: &CliSurfaceProfile,
    client: AiClientProfile,
    install_paths: &InstallPaths,
    skills_path: &Path,
) -> Option<GeneratedArtifact> {
    let spec = host_profile_spec(client);
    let target_path = install_paths.host_config_path(client)?;
    let absolute_skills_path = install_paths.resolve_skills_path(skills_path);
    let server_name = format!("sxmc-cli-ai-{}", slugify(&profile.command));
    let content = render_client_config(client, &server_name, &absolute_skills_path);
    let apply_strategy = match spec.config_shape {
        Some(ConfigShape::JsonMcpServers) | Some(ConfigShape::JsonMcp) => {
            ApplyStrategy::JsonMcpConfig
        }
        Some(ConfigShape::TomlMcpServers) => ApplyStrategy::TomlManagedBlock,
        None => ApplyStrategy::SidecarOnly,
    };

    Some(GeneratedArtifact {
        label: format!("{} client config", spec.label),
        target_path,
        content,
        apply_strategy,
        audience: ArtifactAudience::Client(client),
        sidecar_scope: spec.sidecar_scope.into(),
    })
}

pub fn generate_skill_artifacts(
    profile: &CliSurfaceProfile,
    root: &Path,
    output_dir: &Path,
) -> Vec<GeneratedArtifact> {
    let slug = slugify(&profile.command);
    let skill_dir = if output_dir.is_absolute() {
        output_dir.join(format!("{slug}-cli"))
    } else {
        root.join(output_dir).join(format!("{slug}-cli"))
    };

    vec![GeneratedArtifact {
        label: "Skill scaffold".into(),
        target_path: skill_dir.join("SKILL.md"),
        content: render_skill_markdown(profile),
        apply_strategy: ApplyStrategy::DirectWrite,
        audience: ArtifactAudience::Shared,
        sidecar_scope: "skills".into(),
    }]
}

pub fn generate_ci_workflow_artifact(
    profile: &CliSurfaceProfile,
    root: &Path,
    output_dir: &Path,
) -> GeneratedArtifact {
    let slug = slugify(&profile.command);
    let workflow_dir = if output_dir.is_absolute() {
        output_dir.to_path_buf()
    } else {
        root.join(output_dir)
    };
    GeneratedArtifact {
        label: "CI drift workflow".into(),
        target_path: workflow_dir.join(format!("sxmc-drift-{slug}.yml")),
        content: render_ci_workflow(profile),
        apply_strategy: ApplyStrategy::DirectWrite,
        audience: ArtifactAudience::Shared,
        sidecar_scope: "ci".into(),
    }
}

pub fn generate_mcp_wrapper_artifacts(
    profile: &CliSurfaceProfile,
    root: &Path,
    output_dir: &Path,
) -> Result<Vec<GeneratedArtifact>> {
    let slug = slugify(&profile.command);
    let wrapper_dir = if output_dir.is_absolute() {
        output_dir.join(format!("{slug}-mcp-wrapper"))
    } else {
        root.join(output_dir).join(format!("{slug}-mcp-wrapper"))
    };
    let manifest = json!({
        "name": format!("{slug}-mcp-wrapper"),
        "source_command": profile.command,
        "summary": profile.summary,
        "notes": [
            "Wrap the CLI as a focused MCP server instead of mirroring every subcommand.",
            "Prefer a few narrow tools first and keep outputs machine-friendly.",
            "Use the profile and examples to decide what becomes a tool, prompt, or resource."
        ],
        "suggested_tools": profile.subcommands.iter().take(5).map(|subcommand| {
            json!({
                "name": subcommand.name,
                "summary": subcommand.summary,
                "confidence": subcommand.confidence
            })
        }).collect::<Vec<_>>(),
        "environment": profile.environment,
        "examples": profile.examples,
    });

    Ok(vec![
        GeneratedArtifact {
            label: "MCP wrapper README".into(),
            target_path: wrapper_dir.join("README.md"),
            content: render_mcp_wrapper_readme(profile),
            apply_strategy: ApplyStrategy::DirectWrite,
            audience: ArtifactAudience::Shared,
            sidecar_scope: "mcp-wrapper".into(),
        },
        GeneratedArtifact {
            label: "MCP wrapper manifest".into(),
            target_path: wrapper_dir.join("manifest.json"),
            content: serde_json::to_string_pretty(&manifest)?,
            apply_strategy: ApplyStrategy::DirectWrite,
            audience: ArtifactAudience::Shared,
            sidecar_scope: "mcp-wrapper".into(),
        },
    ])
}

pub fn generate_llms_txt_artifact(profile: &CliSurfaceProfile, root: &Path) -> GeneratedArtifact {
    GeneratedArtifact {
        label: "llms.txt export".into(),
        target_path: root.join("llms.txt"),
        content: render_llms_txt(profile),
        apply_strategy: ApplyStrategy::DirectWrite,
        audience: ArtifactAudience::Shared,
        sidecar_scope: "llms".into(),
    }
}

pub fn materialize_artifacts(
    artifacts: &[GeneratedArtifact],
    mode: ArtifactMode,
    install_paths: &InstallPaths,
) -> Result<Vec<WriteOutcome>> {
    let mut outcomes = Vec::new();
    for artifact in artifacts {
        match mode {
            ArtifactMode::Preview => {
                println!(
                    "== {} ==\nTarget: {}\n\n{}\n",
                    artifact.label,
                    artifact.target_path.display(),
                    artifact.content.trim_end()
                );
                outcomes.push(WriteOutcome {
                    label: artifact.label.clone(),
                    path: artifact.target_path.clone(),
                    mode,
                    status: WriteStatus::Skipped,
                });
            }
            ArtifactMode::WriteSidecar => {
                let path =
                    install_paths.sidecar_path(&artifact.sidecar_scope, &artifact.target_path);
                let status = write_with_status(&path, &artifact.content)?;
                outcomes.push(WriteOutcome {
                    label: artifact.label.clone(),
                    path,
                    mode,
                    status,
                });
            }
            ArtifactMode::Patch => {
                println!("{}", render_patch_preview(artifact, install_paths)?);
                outcomes.push(WriteOutcome {
                    label: artifact.label.clone(),
                    path: artifact.target_path.clone(),
                    mode,
                    status: WriteStatus::Skipped,
                });
            }
            ArtifactMode::Apply => {
                let (path, status) = apply_artifact(artifact, install_paths)?;
                outcomes.push(WriteOutcome {
                    label: artifact.label.clone(),
                    path,
                    mode,
                    status,
                });
            }
        }
    }
    Ok(outcomes)
}

pub fn preview_artifacts(
    artifacts: &[GeneratedArtifact],
    mode: ArtifactMode,
    install_paths: &InstallPaths,
) -> Result<Vec<WriteOutcome>> {
    let mut outcomes = Vec::new();
    for artifact in artifacts {
        let (path, status) = planned_artifact_write(artifact, mode, install_paths)?;
        outcomes.push(WriteOutcome {
            label: artifact.label.clone(),
            path,
            mode,
            status,
        });
    }
    Ok(outcomes)
}

pub fn materialize_artifacts_with_apply_selection(
    artifacts: &[GeneratedArtifact],
    mode: ArtifactMode,
    install_paths: &InstallPaths,
    selected_clients: &[AiClientProfile],
) -> Result<Vec<WriteOutcome>> {
    let mut outcomes = Vec::new();
    for artifact in artifacts {
        let effective_mode = if mode == ArtifactMode::Apply {
            match artifact.audience {
                ArtifactAudience::Shared => ArtifactMode::Apply,
                ArtifactAudience::Portable => {
                    if selected_clients.is_empty() {
                        ArtifactMode::WriteSidecar
                    } else {
                        ArtifactMode::Apply
                    }
                }
                ArtifactAudience::Client(client) => {
                    if selected_clients.contains(&client) {
                        ArtifactMode::Apply
                    } else {
                        ArtifactMode::WriteSidecar
                    }
                }
            }
        } else {
            mode
        };

        outcomes.extend(materialize_artifacts(
            std::slice::from_ref(artifact),
            effective_mode,
            install_paths,
        )?);
    }
    Ok(outcomes)
}

pub fn preview_artifacts_with_apply_selection(
    artifacts: &[GeneratedArtifact],
    mode: ArtifactMode,
    install_paths: &InstallPaths,
    selected_clients: &[AiClientProfile],
) -> Result<Vec<WriteOutcome>> {
    let mut outcomes = Vec::new();
    for artifact in artifacts {
        let effective_mode = if mode == ArtifactMode::Apply {
            match artifact.audience {
                ArtifactAudience::Shared => ArtifactMode::Apply,
                ArtifactAudience::Portable => {
                    if selected_clients.is_empty() {
                        ArtifactMode::WriteSidecar
                    } else {
                        ArtifactMode::Apply
                    }
                }
                ArtifactAudience::Client(client) => {
                    if selected_clients.contains(&client) {
                        ArtifactMode::Apply
                    } else {
                        ArtifactMode::WriteSidecar
                    }
                }
            }
        } else {
            mode
        };

        outcomes.extend(preview_artifacts(
            std::slice::from_ref(artifact),
            effective_mode,
            install_paths,
        )?);
    }
    Ok(outcomes)
}

pub fn remove_artifacts(
    artifacts: &[GeneratedArtifact],
    mode: ArtifactMode,
    install_paths: &InstallPaths,
) -> Result<Vec<WriteOutcome>> {
    let mut outcomes = Vec::new();
    for artifact in artifacts {
        match mode {
            ArtifactMode::Preview => {
                println!(
                    "Would remove {}: {}",
                    artifact.label,
                    artifact.target_path.display()
                );
                outcomes.push(WriteOutcome {
                    label: artifact.label.clone(),
                    path: artifact.target_path.clone(),
                    mode,
                    status: WriteStatus::Skipped,
                });
            }
            ArtifactMode::WriteSidecar => {
                let path =
                    install_paths.sidecar_path(&artifact.sidecar_scope, &artifact.target_path);
                remove_path_if_exists(&path)?;
                outcomes.push(WriteOutcome {
                    label: artifact.label.clone(),
                    path,
                    mode,
                    status: WriteStatus::Removed,
                });
            }
            ArtifactMode::Patch => {
                println!("{}", render_remove_patch_preview(artifact, install_paths)?);
                outcomes.push(WriteOutcome {
                    label: artifact.label.clone(),
                    path: artifact.target_path.clone(),
                    mode,
                    status: WriteStatus::Skipped,
                });
            }
            ArtifactMode::Apply => {
                let path = remove_artifact(artifact, install_paths)?;
                outcomes.push(WriteOutcome {
                    label: artifact.label.clone(),
                    path,
                    mode,
                    status: WriteStatus::Removed,
                });
            }
        }
    }
    Ok(outcomes)
}

pub fn remove_artifacts_with_apply_selection(
    artifacts: &[GeneratedArtifact],
    mode: ArtifactMode,
    install_paths: &InstallPaths,
    selected_clients: &[AiClientProfile],
) -> Result<Vec<WriteOutcome>> {
    let mut outcomes = Vec::new();
    for artifact in artifacts {
        let effective_mode = if mode == ArtifactMode::Apply {
            match artifact.audience {
                ArtifactAudience::Shared => ArtifactMode::Apply,
                ArtifactAudience::Portable => {
                    if selected_clients.is_empty() {
                        ArtifactMode::WriteSidecar
                    } else {
                        ArtifactMode::Apply
                    }
                }
                ArtifactAudience::Client(client) => {
                    if selected_clients.contains(&client) {
                        ArtifactMode::Apply
                    } else {
                        ArtifactMode::WriteSidecar
                    }
                }
            }
        } else {
            mode
        };

        outcomes.extend(remove_artifacts(
            std::slice::from_ref(artifact),
            effective_mode,
            install_paths,
        )?);
    }
    Ok(outcomes)
}

fn render_patch_preview(
    artifact: &GeneratedArtifact,
    install_paths: &InstallPaths,
) -> Result<String> {
    let existing = if artifact.target_path.exists() {
        fs::read_to_string(&artifact.target_path)?
    } else {
        String::new()
    };
    let proposed = proposed_applied_content(artifact, install_paths)?;
    Ok(format!(
        "--- {}\n+++ {}\n{}\n",
        artifact.target_path.display(),
        artifact.target_path.display(),
        render_patch_body(&existing, &proposed)
    ))
}

fn render_remove_patch_preview(
    artifact: &GeneratedArtifact,
    install_paths: &InstallPaths,
) -> Result<String> {
    let existing = if artifact.target_path.exists() {
        fs::read_to_string(&artifact.target_path)?
    } else {
        String::new()
    };
    let proposed = proposed_removed_content(artifact, install_paths)?;
    Ok(format!(
        "--- {}\n+++ {}\n{}\n",
        artifact.target_path.display(),
        artifact.target_path.display(),
        render_patch_body(&existing, &proposed)
    ))
}

fn proposed_applied_content(
    artifact: &GeneratedArtifact,
    _install_paths: &InstallPaths,
) -> Result<String> {
    match artifact.apply_strategy {
        ApplyStrategy::ManagedMarkdownBlock => {
            let existing = if artifact.target_path.exists() {
                fs::read_to_string(&artifact.target_path)?
            } else {
                String::new()
            };
            Ok(upsert_managed_block(
                &existing,
                &artifact.content,
                markdown_block_markers(artifact),
            ))
        }
        ApplyStrategy::JsonMcpConfig => {
            let existing = if artifact.target_path.exists() {
                fs::read_to_string(&artifact.target_path)?
            } else {
                String::new()
            };
            merge_json_mcp_config(&existing, &artifact.content)
        }
        ApplyStrategy::TomlManagedBlock => {
            let existing = if artifact.target_path.exists() {
                fs::read_to_string(&artifact.target_path)?
            } else {
                String::new()
            };
            merge_toml_mcp_config(
                &existing,
                &artifact.content,
                &toml_block_markers(artifact),
                &artifact.target_path,
            )
        }
        ApplyStrategy::DirectWrite => Ok(artifact.content.clone()),
        ApplyStrategy::SidecarOnly => Ok(artifact.content.clone()),
    }
}

fn proposed_removed_content(
    artifact: &GeneratedArtifact,
    install_paths: &InstallPaths,
) -> Result<String> {
    match artifact.apply_strategy {
        ApplyStrategy::SidecarOnly => {
            let _path = install_paths.sidecar_path(&artifact.sidecar_scope, &artifact.target_path);
            Ok(String::new())
        }
        ApplyStrategy::ManagedMarkdownBlock => {
            let existing = if artifact.target_path.exists() {
                fs::read_to_string(&artifact.target_path)?
            } else {
                String::new()
            };
            Ok(remove_managed_block(
                &existing,
                markdown_block_markers(artifact),
            ))
        }
        ApplyStrategy::JsonMcpConfig => {
            let existing = if artifact.target_path.exists() {
                fs::read_to_string(&artifact.target_path)?
            } else {
                String::new()
            };
            remove_json_mcp_config(&existing, &artifact.content)
        }
        ApplyStrategy::TomlManagedBlock => {
            let existing = if artifact.target_path.exists() {
                fs::read_to_string(&artifact.target_path)?
            } else {
                String::new()
            };
            remove_toml_mcp_config(
                &existing,
                &artifact.content,
                &toml_block_markers(artifact),
                &artifact.target_path,
            )
        }
        ApplyStrategy::DirectWrite => Ok(String::new()),
    }
}

fn render_patch_body(existing: &str, proposed: &str) -> String {
    let old_lines: Vec<&str> = existing.lines().collect();
    let new_lines: Vec<&str> = proposed.lines().collect();
    let mut body = String::new();
    for line in &old_lines {
        body.push('-');
        body.push_str(line);
        body.push('\n');
    }
    for line in &new_lines {
        body.push('+');
        body.push_str(line);
        body.push('\n');
    }
    body
}

fn apply_artifact(
    artifact: &GeneratedArtifact,
    install_paths: &InstallPaths,
) -> Result<(PathBuf, WriteStatus)> {
    match artifact.apply_strategy {
        ApplyStrategy::SidecarOnly => {
            let path = install_paths.sidecar_path(&artifact.sidecar_scope, &artifact.target_path);
            let status = write_with_status(&path, &artifact.content)?;
            Ok((path, status))
        }
        ApplyStrategy::TomlManagedBlock => {
            let status = update_codex_config(artifact, false, lock_codex_config)?;
            Ok((artifact.target_path.clone(), status))
        }
        ApplyStrategy::ManagedMarkdownBlock | ApplyStrategy::JsonMcpConfig => {
            let updated = proposed_applied_content(artifact, install_paths)?;
            let status = write_with_status(&artifact.target_path, &updated)?;
            Ok((artifact.target_path.clone(), status))
        }
        ApplyStrategy::DirectWrite => {
            let status = write_with_status(&artifact.target_path, &artifact.content)?;
            Ok((artifact.target_path.clone(), status))
        }
    }
}

fn planned_artifact_write(
    artifact: &GeneratedArtifact,
    mode: ArtifactMode,
    install_paths: &InstallPaths,
) -> Result<(PathBuf, WriteStatus)> {
    match mode {
        ArtifactMode::Preview | ArtifactMode::Patch => {
            Ok((artifact.target_path.clone(), WriteStatus::Skipped))
        }
        ArtifactMode::WriteSidecar => {
            let path = install_paths.sidecar_path(&artifact.sidecar_scope, &artifact.target_path);
            let status = planned_write_status(&path, &artifact.content)?;
            Ok((path, status))
        }
        ArtifactMode::Apply => match artifact.apply_strategy {
            ApplyStrategy::SidecarOnly => {
                let path =
                    install_paths.sidecar_path(&artifact.sidecar_scope, &artifact.target_path);
                let status = planned_write_status(&path, &artifact.content)?;
                Ok((path, status))
            }
            ApplyStrategy::ManagedMarkdownBlock
            | ApplyStrategy::JsonMcpConfig
            | ApplyStrategy::TomlManagedBlock => {
                let proposed = proposed_applied_content(artifact, install_paths)?;
                let status = planned_write_status(&artifact.target_path, &proposed)?;
                Ok((artifact.target_path.clone(), status))
            }
            ApplyStrategy::DirectWrite => {
                let status = planned_write_status(&artifact.target_path, &artifact.content)?;
                Ok((artifact.target_path.clone(), status))
            }
        },
    }
}

fn remove_artifact(artifact: &GeneratedArtifact, install_paths: &InstallPaths) -> Result<PathBuf> {
    match artifact.apply_strategy {
        ApplyStrategy::SidecarOnly => {
            let path = install_paths.sidecar_path(&artifact.sidecar_scope, &artifact.target_path);
            remove_path_if_exists(&path)?;
            Ok(path)
        }
        ApplyStrategy::ManagedMarkdownBlock => {
            if !artifact.target_path.exists() {
                return Ok(artifact.target_path.clone());
            }
            let existing = fs::read_to_string(&artifact.target_path)?;
            let updated = remove_managed_block(&existing, markdown_block_markers(artifact));
            write_or_remove_target(&artifact.target_path, &updated)?;
            Ok(artifact.target_path.clone())
        }
        ApplyStrategy::JsonMcpConfig => {
            if !artifact.target_path.exists() {
                return Ok(artifact.target_path.clone());
            }
            let existing = fs::read_to_string(&artifact.target_path)?;
            let updated = remove_json_mcp_config(&existing, &artifact.content)?;
            write_or_remove_target(&artifact.target_path, &updated)?;
            Ok(artifact.target_path.clone())
        }
        ApplyStrategy::TomlManagedBlock => {
            update_codex_config(artifact, true, lock_codex_config)?;
            Ok(artifact.target_path.clone())
        }
        ApplyStrategy::DirectWrite => {
            remove_path_if_exists(&artifact.target_path)?;
            Ok(artifact.target_path.clone())
        }
    }
}

fn regular_file_metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata)),
        Ok(_) => Err(SxmcError::Other(format!(
            "Refusing to modify Codex config or lock '{}': expected a regular file, with no symlink",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn lock_codex_config(path: &Path) -> Result<File> {
    let lock_path = path.with_file_name("config.toml.sxmc.lock");
    regular_file_metadata(&lock_path)?;
    // Keep this file in place so a second writer locks the same inode after config replacement.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            SxmcError::Other(format!(
                "Cannot open Codex config lock '{}': {error}",
                lock_path.display()
            ))
        })?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(SxmcError::Other(format!(
                        "Timed out waiting for Codex config lock '{}'. Retry after the other writer finishes.",
                        lock_path.display()
                    )));
                }
                std::thread::sleep(remaining.min(Duration::from_millis(25)));
            }
            Err(TryLockError::Error(error)) => {
                return Err(SxmcError::Other(format!(
                    "Cannot acquire Codex config lock '{}': {error}",
                    lock_path.display()
                )));
            }
        }
    }
}

fn update_codex_config(
    artifact: &GeneratedArtifact,
    remove: bool,
    acquire_lock: impl FnOnce(&Path) -> Result<File>,
) -> Result<WriteStatus> {
    let target = &artifact.target_path;
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !remove {
        fs::create_dir_all(parent)?;
    }
    let parent = match fs::canonicalize(parent) {
        Ok(parent) => parent,
        Err(error) if remove && error.kind() == io::ErrorKind::NotFound => {
            return Ok(WriteStatus::Skipped);
        }
        Err(error) => return Err(error.into()),
    };
    let name = target.file_name().ok_or_else(|| {
        SxmcError::Other(format!(
            "Missing Codex config filename in '{}'",
            target.display()
        ))
    })?;
    let path = parent.join(name);
    let _lock = acquire_lock(&path)?;
    let metadata = regular_file_metadata(&path)?;
    let existing = match &metadata {
        Some(_) => fs::read_to_string(&path)?,
        None if remove => return Ok(WriteStatus::Skipped),
        None => String::new(),
    };
    let markers = toml_block_markers(artifact);
    let updated = if remove {
        remove_toml_mcp_config(&existing, &artifact.content, &markers, &path)?
    } else {
        merge_toml_mcp_config(&existing, &artifact.content, &markers, &path)?
    };
    if updated == existing {
        return Ok(WriteStatus::Skipped);
    }
    if remove && updated.trim().is_empty() {
        regular_file_metadata(&path)?;
        fs::remove_file(&path)?;
        return Ok(WriteStatus::Removed);
    }

    let mut staged = tempfile::NamedTempFile::new_in(&parent)?;
    staged.write_all(updated.as_bytes())?;
    if let Some(metadata) = &metadata {
        staged.as_file().set_permissions(metadata.permissions())?;
    }
    staged.as_file().sync_all()?;
    regular_file_metadata(&path)?;
    staged.persist(&path).map_err(|error| {
        SxmcError::Other(format!(
            "Cannot replace Codex config '{}': {}",
            path.display(),
            error.error
        ))
    })?;
    Ok(if metadata.is_some() {
        WriteStatus::Updated
    } else {
        WriteStatus::Created
    })
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

fn write_with_status(path: &Path, content: &str) -> Result<WriteStatus> {
    let existed = path.exists();
    if existed {
        let existing = fs::read_to_string(path).unwrap_or_default();
        if existing == content {
            return Ok(WriteStatus::Skipped);
        }
    }
    write_file(path, content)?;
    Ok(if existed {
        WriteStatus::Updated
    } else {
        WriteStatus::Created
    })
}

fn planned_write_status(path: &Path, content: &str) -> Result<WriteStatus> {
    if path.exists() {
        let existing = fs::read_to_string(path)?;
        if existing == content {
            Ok(WriteStatus::Skipped)
        } else {
            Ok(WriteStatus::Updated)
        }
    } else {
        Ok(WriteStatus::Created)
    }
}

fn write_or_remove_target(path: &Path, content: &str) -> Result<()> {
    if content.trim().is_empty() {
        remove_path_if_exists(path)?;
    } else {
        write_file(path, content)?;
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn markdown_block_markers(artifact: &GeneratedArtifact) -> (String, String) {
    (
        format!("<!-- sxmc:begin cli-ai:{} -->", artifact.sidecar_scope),
        format!("<!-- sxmc:end cli-ai:{} -->", artifact.sidecar_scope),
    )
}

fn toml_block_markers_for_scope(scope: &str) -> (String, String) {
    (
        format!("# sxmc:begin cli-ai:{}", scope),
        format!("# sxmc:end cli-ai:{}", scope),
    )
}

fn toml_block_markers(artifact: &GeneratedArtifact) -> (String, String) {
    toml_block_markers_for_scope(&artifact.sidecar_scope)
}

struct ManagedBlockSpan<'a> {
    before: &'a str,
    inner: &'a str,
    after: &'a str,
}

fn find_marker_lines(text: &str, marker: &str) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    let mut offset = 0usize;
    for chunk in text.split_inclusive('\n') {
        if chunk.trim_end_matches(['\r', '\n']).trim() == marker {
            matches.push((offset, offset + chunk.len()));
        }
        offset += chunk.len();
    }
    matches
}

fn split_toml_managed_block<'a>(
    existing: &'a str,
    markers: &(String, String),
    target_path: &Path,
) -> Result<Option<ManagedBlockSpan<'a>>> {
    let begins = find_marker_lines(existing, &markers.0);
    let ends = find_marker_lines(existing, &markers.1);

    if begins.is_empty() && ends.is_empty() {
        return Ok(None);
    }
    if begins.len() != 1 || ends.len() != 1 {
        return Err(SxmcError::Other(format!(
            "Malformed or duplicate sxmc managed block markers in '{}'",
            target_path.display()
        )));
    }

    let (begin_start, begin_end) = begins[0];
    let (end_start, end_end) = ends[0];
    if end_start < begin_end {
        return Err(SxmcError::Other(format!(
            "Malformed sxmc managed block markers in '{}': end marker appears before begin marker",
            target_path.display()
        )));
    }

    Ok(Some(ManagedBlockSpan {
        before: &existing[..begin_start],
        inner: &existing[begin_end..end_start],
        after: &existing[end_end..],
    }))
}

pub fn has_managed_toml_block(path: &Path, scope: &str) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let markers = toml_block_markers_for_scope(scope);
    matches!(
        split_toml_managed_block(&contents, &markers, path),
        Ok(Some(_))
    )
}

fn parse_toml_table(content: &str, context: &str) -> Result<toml::Table> {
    if content.trim().is_empty() {
        return Ok(toml::Table::new());
    }
    toml::from_str::<toml::Table>(content)
        .map_err(|error| SxmcError::Other(format!("{context} is not valid TOML: {error}")))
}

fn extract_mcp_servers(
    table: &toml::Table,
    context: &str,
) -> Result<BTreeMap<String, toml::Table>> {
    let Some(raw) = table.get("mcp_servers") else {
        return Ok(BTreeMap::new());
    };
    let servers_table = raw.as_table().ok_or_else(|| {
        SxmcError::Other(format!("{context} has a non-table 'mcp_servers' entry"))
    })?;
    let mut out = BTreeMap::new();
    for (name, value) in servers_table {
        let entry = value.as_table().ok_or_else(|| {
            SxmcError::Other(format!(
                "{context} has a non-table 'mcp_servers.{name}' entry"
            ))
        })?;
        out.insert(name.clone(), entry.clone());
    }
    Ok(out)
}

fn parse_managed_toml_inner(
    inner: &str,
    target_path: &Path,
) -> Result<BTreeMap<String, toml::Table>> {
    let context = format!("Existing sxmc managed block in '{}'", target_path.display());
    let table = parse_toml_table(inner, &context)?;
    for key in table.keys() {
        if key != "mcp_servers" {
            return Err(SxmcError::Other(format!(
                "{context} contains unexpected non-MCP key '{key}'"
            )));
        }
    }
    extract_mcp_servers(&table, &context)
}

fn render_codex_server_section(name: &str, server: &toml::Table) -> Result<String> {
    let key = format_toml_key(name);
    if let (Some(command), Some(args)) = (
        server.get("command").and_then(toml::Value::as_str),
        server.get("args").and_then(toml::Value::as_array),
    ) {
        if server.len() == 2 && args.iter().all(toml::Value::is_str) {
            let command_literal = toml::Value::String(command.to_string()).to_string();
            let args_literals = args
                .iter()
                .filter_map(toml::Value::as_str)
                .map(|arg| toml::Value::String(arg.to_string()).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(format!(
                "[mcp_servers.{key}]\ncommand = {command_literal}\nargs = [{args_literals}]\n"
            ));
        }
    }
    let document = toml::Table::from_iter([(
        "mcp_servers".into(),
        toml::Value::Table(toml::Table::from_iter([(
            name.into(),
            toml::Value::Table(server.clone()),
        )])),
    )]);
    toml::to_string(&document).map_err(|error| {
        SxmcError::Other(format!(
            "Failed to serialize Codex MCP server '{name}' as TOML: {error}"
        ))
    })
}

fn render_codex_server_sections(servers: &BTreeMap<String, toml::Table>) -> Result<String> {
    let mut sections = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        sections.push(
            render_codex_server_section(name, server)?
                .trim_end()
                .to_string(),
        );
    }
    Ok(sections.join("\n\n"))
}

fn render_managed_toml_block(
    header_comment: &str,
    servers: &BTreeMap<String, toml::Table>,
    markers: &(String, String),
) -> Result<String> {
    let sections = render_codex_server_sections(servers)?;
    Ok(format!(
        "{}\n{}\n{sections}\n{}\n",
        markers.0, header_comment, markers.1
    ))
}

fn join_removed_block_parts(before: &str, after: &str) -> String {
    let before_trimmed = before.trim_end_matches(['\r', '\n']);
    let after_trimmed = after.trim_start_matches(['\r', '\n']);
    match (before_trimmed.is_empty(), after_trimmed.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("{before_trimmed}\n"),
        (true, false) => after_trimmed.to_string(),
        (false, false) => format!("{before_trimmed}\n\n{after_trimmed}"),
    }
}

fn splice_managed_block(before: &str, block: &str, after: &str) -> String {
    let mut updated = String::new();
    updated.push_str(before);
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(block);
    if !after.is_empty() {
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(after);
    }
    updated
}

fn same_toml_table(left: &toml::Table, right: &toml::Table) -> bool {
    left.len() == right.len()
        && left.iter().all(|(key, value)| {
            right
                .get(key)
                .is_some_and(|other| same_toml_value(value, other))
        })
}

fn same_toml_value(left: &toml::Value, right: &toml::Value) -> bool {
    match (left, right) {
        (toml::Value::Table(left), toml::Value::Table(right)) => same_toml_table(left, right),
        (toml::Value::Array(left), toml::Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_toml_value(left, right))
        }
        (toml::Value::Float(left), toml::Value::Float(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

fn codex_user_settings_conflict(path: &Path, detail: &str) -> SxmcError {
    SxmcError::Other(format!(
        "Conflict updating Codex config '{}': cannot preserve user settings ({detail}). Give user-owned settings an explicit table outside sxmc's managed block, or move them to a separate server before retrying.",
        path.display()
    ))
}

fn validate_codex_edit(proposed: String, mut expected: toml::Table, path: &Path) -> Result<String> {
    let actual = toml::from_str::<toml::Table>(&proposed).map_err(|error| {
        codex_user_settings_conflict(path, &format!("resulting TOML is invalid: {error}"))
    })?;
    // The outer table disappears with the last managed server unless the user declared it.
    if !actual.contains_key("mcp_servers")
        && expected
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
            .is_some_and(toml::Table::is_empty)
    {
        expected.remove("mcp_servers");
    }
    if !same_toml_table(&expected, &actual) {
        return Err(codex_user_settings_conflict(
            path,
            "an edit would change the scope or value of existing settings",
        ));
    }
    Ok(proposed)
}

fn edit_managed_codex_servers(
    inner: &str,
    path: &Path,
    edit: impl FnOnce(&mut dyn toml_edit::TableLike) -> Result<()>,
) -> Result<String> {
    // Keep the original TOML table positions. The final table can own settings after the marker.
    let mut document = inner.parse::<toml_edit::DocumentMut>().map_err(|error| {
        codex_user_settings_conflict(path, &format!("cannot edit managed TOML: {error}"))
    })?;
    let servers = document
        .get_mut("mcp_servers")
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or_else(|| {
            codex_user_settings_conflict(path, "cannot locate the managed server table")
        })?;
    edit(servers)?;
    Ok(document.to_string())
}

fn merge_toml_mcp_config(
    existing: &str,
    generated: &str,
    markers: &(String, String),
    target_path: &Path,
) -> Result<String> {
    let target_context = format!("Existing Codex config '{}'", target_path.display());
    let mut expected = parse_toml_table(existing, &target_context)?;

    let generated_table = parse_toml_table(generated, "Generated Codex config")?;
    let generated_servers = extract_mcp_servers(&generated_table, "Generated Codex config")?;
    if generated_servers.is_empty() {
        return Err(SxmcError::Other(
            "Generated Codex config missing [mcp_servers.<name>] entry".into(),
        ));
    }
    let header_comment = generated
        .lines()
        .find(|line| line.trim_start().starts_with('#'))
        .map(str::trim_end)
        .unwrap_or("# sxmc CLI->AI startup scaffold");

    let span = split_toml_managed_block(existing, markers, target_path)?;
    let managed = match &span {
        Some(span) => parse_managed_toml_inner(span.inner, target_path)?,
        None => BTreeMap::new(),
    };
    let servers = expected
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| {
            SxmcError::Other(format!(
                "{target_context} has a non-table 'mcp_servers' entry"
            ))
        })?;

    let mut additions = BTreeMap::new();
    let mut replacements = BTreeMap::<_, toml::Table>::new();
    // Change only generated keys in the full parsed document. Marker comments have no TOML scope.
    for (name, generated_server) in generated_servers {
        if let Some(owned) = managed.get(&name) {
            let full_server = servers
                .get_mut(&name)
                .and_then(toml::Value::as_table_mut)
                .ok_or_else(|| {
                    codex_user_settings_conflict(
                        target_path,
                        &format!("cannot locate managed server '{name}'"),
                    )
                })?;
            for (key, value) in generated_server {
                match (owned.get(&key), full_server.get(&key)) {
                    (Some(old), Some(full)) if same_toml_value(old, full) => {
                        if same_toml_value(old, &value) {
                            continue;
                        }
                    }
                    (None, None) => {}
                    _ => {
                        return Err(codex_user_settings_conflict(
                            target_path,
                            &format!("'{name}.{key}' overlaps a setting outside the managed block"),
                        ))
                    }
                }
                full_server.insert(key.clone(), value.clone());
                replacements
                    .entry(name.clone())
                    .or_default()
                    .insert(key, value);
            }
        } else {
            if servers.contains_key(&name) {
                return Err(SxmcError::Other(format!(
                    "Refusing to overwrite unmanaged Codex MCP server '[mcp_servers.{name}]' in '{}' outside sxmc's managed block",
                    target_path.display()
                )));
            }
            servers.insert(name.clone(), toml::Value::Table(generated_server.clone()));
            additions.insert(name, generated_server);
        }
    }

    let proposed = match span {
        Some(_) if replacements.is_empty() && additions.is_empty() => {
            return Ok(existing.to_string())
        }
        Some(span) => {
            let mut inner = if replacements.is_empty() {
                span.inner.to_string()
            } else {
                edit_managed_codex_servers(span.inner, target_path, |servers| {
                    for (name, replacement) in replacements {
                        let server = servers
                            .get_mut(&name)
                            .and_then(toml_edit::Item::as_table_like_mut)
                            .ok_or_else(|| {
                                codex_user_settings_conflict(
                                    target_path,
                                    &format!("cannot edit managed server '{name}'"),
                                )
                            })?;
                        for (key, value) in replacement {
                            let mut value =
                                value
                                    .to_string()
                                    .parse::<toml_edit::Value>()
                                    .map_err(|error| {
                                        codex_user_settings_conflict(
                                            target_path,
                                            &format!("cannot render '{name}.{key}': {error}"),
                                        )
                                    })?;
                            if let Some(old) = server.get(&key).and_then(toml_edit::Item::as_value)
                            {
                                *value.decor_mut() = old.decor().clone();
                            }
                            server.insert(&key, toml_edit::Item::Value(value));
                        }
                    }
                    Ok(())
                })?
            };
            if !additions.is_empty() {
                // Insert new tables ahead of the original final table.
                let sections = render_codex_server_sections(&additions)?;
                inner = format!("{sections}\n\n{inner}");
            }
            let block = format!("{}\n{inner}{}\n", markers.0, markers.1);
            splice_managed_block(span.before, &block, span.after)
        }
        None => {
            let block = render_managed_toml_block(header_comment, &additions, markers)?;
            if existing.is_empty() {
                block
            } else {
                format!("{}\n\n{block}", existing.trim_end_matches(['\r', '\n']))
            }
        }
    };

    validate_codex_edit(proposed, expected, target_path)
}

fn remove_toml_mcp_config(
    existing: &str,
    generated: &str,
    markers: &(String, String),
    target_path: &Path,
) -> Result<String> {
    if existing.trim().is_empty() {
        return Ok(String::new());
    }
    let target_context = format!("Existing Codex config '{}'", target_path.display());
    let mut expected = parse_toml_table(existing, &target_context)?;

    let Some(span) = split_toml_managed_block(existing, markers, target_path)? else {
        return Ok(existing.to_string());
    };

    let generated_table = parse_toml_table(generated, "Generated Codex config")?;
    let generated_servers = extract_mcp_servers(&generated_table, "Generated Codex config")?;
    let managed = parse_managed_toml_inner(span.inner, target_path)?;
    let Some(servers) = expected
        .get_mut("mcp_servers")
        .and_then(toml::Value::as_table_mut)
    else {
        return Err(codex_user_settings_conflict(
            target_path,
            "cannot locate the managed server table",
        ));
    };
    let mut removed = Vec::new();
    for (name, owned) in managed
        .iter()
        .filter(|(name, _)| generated_servers.contains_key(*name))
    {
        let full_server = servers.get(name).and_then(toml::Value::as_table);
        if !full_server.is_some_and(|full| same_toml_table(full, owned)) {
            return Err(codex_user_settings_conflict(
                target_path,
                &format!("server '{name}' has settings outside the managed block"),
            ));
        }
        servers.remove(name);
        removed.push(name);
    }
    if removed.is_empty() {
        return Ok(existing.to_string());
    }

    let updated = if removed.len() == managed.len() {
        join_removed_block_parts(span.before, span.after)
    } else {
        let inner = edit_managed_codex_servers(span.inner, target_path, |servers| {
            for name in removed {
                servers.remove(name);
            }
            Ok(())
        })?;
        let block = format!("{}\n{inner}{}\n", markers.0, markers.1);
        splice_managed_block(span.before, &block, span.after)
    };

    validate_codex_edit(updated, expected, target_path)
}

fn upsert_managed_block(existing: &str, new_content: &str, markers: (String, String)) -> String {
    let block = format!("{}\n{}\n{}\n", markers.0, new_content.trim_end(), markers.1);
    if let (Some(start), Some(end)) = (existing.find(&markers.0), existing.find(&markers.1)) {
        let mut updated = String::new();
        updated.push_str(&existing[..start]);
        if !updated.ends_with('\n') && !updated.is_empty() {
            updated.push('\n');
        }
        updated.push_str(&block);
        let after = &existing[end + markers.1.len()..];
        if !after.is_empty() {
            if !updated.ends_with('\n') {
                updated.push('\n');
            }
            updated.push_str(after.trim_start_matches('\n'));
        }
        return updated;
    }

    if existing.trim().is_empty() {
        return block;
    }

    let mut updated = existing.trim_end().to_string();
    updated.push_str("\n\n");
    updated.push_str(&block);
    updated
}

fn remove_managed_block(existing: &str, markers: (String, String)) -> String {
    if let (Some(start), Some(end)) = (existing.find(&markers.0), existing.find(&markers.1)) {
        let before = existing[..start].trim_end_matches('\n');
        let after = existing[end + markers.1.len()..].trim_start_matches('\n');
        let mut updated = String::new();
        if !before.is_empty() {
            updated.push_str(before);
        }
        if !before.is_empty() && !after.is_empty() {
            updated.push_str("\n\n");
        }
        if !after.is_empty() {
            updated.push_str(after);
        }
        return updated;
    }

    existing.to_string()
}

fn merge_json_mcp_config(existing: &str, generated: &str) -> Result<String> {
    let generated_value = serde_json::from_str::<Value>(generated)?;
    let root_key = if generated_value.get("mcpServers").is_some() {
        "mcpServers"
    } else if generated_value.get("mcp").is_some() {
        "mcp"
    } else {
        return Err(SxmcError::Other(
            "Generated config missing mcpServers or mcp object".into(),
        ));
    };

    let mut base = if existing.trim().is_empty() {
        json!({ root_key: {} })
    } else {
        serde_json::from_str::<Value>(existing)?
    };

    let generated_servers = generated_value
        .get(root_key)
        .and_then(Value::as_object)
        .ok_or_else(|| SxmcError::Other(format!("Generated config missing {} object", root_key)))?
        .clone();

    let root_obj = base
        .as_object_mut()
        .ok_or_else(|| SxmcError::Other("Existing config is not a JSON object".into()))?;
    if !root_obj.contains_key(root_key) {
        root_obj.insert(root_key.into(), Value::Object(Map::new()));
    }
    let servers = root_obj
        .get_mut(root_key)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            SxmcError::Other(format!(
                "Existing config has a non-object {} value",
                root_key
            ))
        })?;

    for (name, config) in generated_servers {
        servers.insert(name, config);
    }

    serde_json::to_string_pretty(&base).map_err(Into::into)
}

fn remove_json_mcp_config(existing: &str, generated: &str) -> Result<String> {
    if existing.trim().is_empty() {
        return Ok(String::new());
    }

    let generated_value = serde_json::from_str::<Value>(generated)?;
    let root_key = if generated_value.get("mcpServers").is_some() {
        "mcpServers"
    } else if generated_value.get("mcp").is_some() {
        "mcp"
    } else {
        return Err(SxmcError::Other(
            "Generated config missing mcpServers or mcp object".into(),
        ));
    };

    let mut base = serde_json::from_str::<Value>(existing)?;
    let generated_servers = generated_value
        .get(root_key)
        .and_then(Value::as_object)
        .ok_or_else(|| SxmcError::Other(format!("Generated config missing {} object", root_key)))?;

    let root_obj = match base.as_object_mut() {
        Some(root) => root,
        None => return Ok(existing.to_string()),
    };

    let Some(servers) = root_obj.get_mut(root_key).and_then(Value::as_object_mut) else {
        return Ok(existing.to_string());
    };

    for name in generated_servers.keys() {
        servers.remove(name);
    }

    if servers.is_empty() {
        root_obj.remove(root_key);
    }

    if root_obj.is_empty() {
        Ok(String::new())
    } else {
        serde_json::to_string_pretty(&base).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::cli_surfaces::model::{AI_HOST_SPECS, CLI_AI_HOSTS_LAST_VERIFIED};
    use crate::paths::InstallPaths;

    #[test]
    fn merge_markdown_block_preserves_existing_content() {
        let existing = "# Repo\n\nExisting text.\n";
        let artifact = GeneratedArtifact {
            label: "Portable agent doc".into(),
            target_path: PathBuf::from("AGENTS.md"),
            content: String::new(),
            apply_strategy: ApplyStrategy::ManagedMarkdownBlock,
            audience: ArtifactAudience::Portable,
            sidecar_scope: "portable".into(),
        };
        let updated = upsert_managed_block(
            existing,
            "## Generated\ncontent",
            markdown_block_markers(&artifact),
        );
        assert!(updated.contains("Existing text."));
        assert!(updated.contains("<!-- sxmc:begin cli-ai:portable -->"));
        assert!(updated.contains("## Generated"));
    }

    #[test]
    fn merge_markdown_blocks_with_different_scopes_coexist() {
        let portable = GeneratedArtifact {
            label: "Portable agent doc".into(),
            target_path: PathBuf::from("AGENTS.md"),
            content: String::new(),
            apply_strategy: ApplyStrategy::ManagedMarkdownBlock,
            audience: ArtifactAudience::Portable,
            sidecar_scope: "portable".into(),
        };
        let codex = GeneratedArtifact {
            label: "OpenAI Codex agent doc".into(),
            target_path: PathBuf::from("AGENTS.md"),
            content: String::new(),
            apply_strategy: ApplyStrategy::ManagedMarkdownBlock,
            audience: ArtifactAudience::Client(AiClientProfile::OpenaiCodex),
            sidecar_scope: "openai-codex".into(),
        };

        let first = upsert_managed_block("", "## Portable", markdown_block_markers(&portable));
        let second = upsert_managed_block(&first, "## Codex", markdown_block_markers(&codex));

        assert!(second.contains("<!-- sxmc:begin cli-ai:portable -->"));
        assert!(second.contains("<!-- sxmc:begin cli-ai:openai-codex -->"));
        assert!(second.contains("## Portable"));
        assert!(second.contains("## Codex"));
    }

    #[test]
    fn merge_json_config_preserves_existing_servers() {
        let existing = r#"{"mcpServers":{"existing":{"command":"foo","args":[]}}}"#;
        let generated = r#"{"mcpServers":{"sxmc-cli-ai-gh":{"command":"sxmc","args":["serve"]}}}"#;
        let merged = merge_json_mcp_config(existing, generated).unwrap();
        assert!(merged.contains("\"existing\""));
        assert!(merged.contains("\"sxmc-cli-ai-gh\""));
    }

    #[test]
    fn merge_json_config_supports_opencode_shape() {
        let existing = r#"{"mcp":{"existing":{"type":"local","command":["foo"]}}}"#;
        let generated = r#"{"mcp":{"sxmc-cli-ai-gh":{"type":"local","command":["sxmc","serve"]}}}"#;
        let merged = merge_json_mcp_config(existing, generated).unwrap();
        assert!(merged.contains("\"existing\""));
        assert!(merged.contains("\"sxmc-cli-ai-gh\""));
    }

    #[test]
    fn generate_client_config_for_all_profiles() {
        let profile: CliSurfaceProfile =
            serde_json::from_str(include_str!("../../examples/profiles/from_cli.json")).unwrap();
        let root = tempdir().unwrap();
        let install_paths = InstallPaths::local(root.path().to_path_buf());
        let skills_path = root.path().join(".claude/skills");

        for spec in AI_HOST_SPECS {
            if let Some(artifact) =
                generate_client_config_artifact(&profile, spec.client, &install_paths, &skills_path)
            {
                assert!(
                    !artifact.content.is_empty(),
                    "{}",
                    CLI_AI_HOSTS_LAST_VERIFIED
                );
            }
        }
    }

    #[test]
    fn host_specs_have_labels_scopes_and_references() {
        for spec in AI_HOST_SPECS {
            assert!(!spec.label.is_empty());
            assert!(!spec.sidecar_scope.is_empty());
            assert!(spec.official_reference_url.starts_with("https://"));
        }
    }

    #[test]
    fn sidecar_write_keeps_real_doc_untouched() {
        let root = tempdir().unwrap();
        let install_paths = InstallPaths::local(root.path().to_path_buf());
        let target = root.path().join("AGENTS.md");
        fs::write(&target, "Existing").unwrap();
        let artifact = GeneratedArtifact {
            label: "Agent doc".into(),
            target_path: target.clone(),
            content: "## Generated".into(),
            apply_strategy: ApplyStrategy::ManagedMarkdownBlock,
            audience: ArtifactAudience::Portable,
            sidecar_scope: "portable".into(),
        };

        let outcomes =
            materialize_artifacts(&[artifact], ArtifactMode::WriteSidecar, &install_paths).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "Existing");
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].path.to_string_lossy().contains(".sxmc"));
    }

    #[test]
    fn toml_render_and_merge_handle_escaped_paths_and_inline_table_conflict() {
        let generated = crate::cli_surfaces::render::render_codex_mcp_server_toml(
            "# sxmc CLI->AI startup scaffold",
            "sxmc-cli-ai-gh",
            "sxmc",
            &[
                "serve".into(),
                "--paths".into(),
                r"C:\Users\dev\.claude\skills".into(),
            ],
        );
        let parsed: toml::Table =
            toml::from_str(&generated).expect("escaped Windows path is valid TOML");
        assert_eq!(
            parsed["mcp_servers"]["sxmc-cli-ai-gh"]["args"][2].as_str(),
            Some(r"C:\Users\dev\.claude\skills")
        );

        let markers = toml_block_markers_for_scope("openai-codex");
        let inline_existing = "mcp_servers = { other = { command = \"foo\" } }\n";
        let err = merge_toml_mcp_config(
            inline_existing,
            &generated,
            &markers,
            Path::new(".codex/config.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("Conflict"));
    }

    #[test]
    fn toml_update_preserves_nested_settings_inside_and_outside_markers() {
        let existing = concat!(
            "sample = [nan, -0.0]\n",
            "# sxmc:begin cli-ai:openai-codex\n",
            "[mcp_servers.'with dot']\ncommand = 'old'\n",
            "[mcp_servers.'with dot'.env]\nOWNED = 'inside'\n",
            "# sxmc:end cli-ai:openai-codex\nUSER = 'outside'\n",
            "[mcp_servers.user.env]\nOTHER = 'unchanged'\n",
        );
        let generated = "[mcp_servers.'with dot']\ncommand = 'new'\nargs = ['serve']\n";
        let updated = merge_toml_mcp_config(
            existing,
            generated,
            &toml_block_markers_for_scope("openai-codex"),
            Path::new(".codex/config.toml"),
        )
        .unwrap();
        assert!(updated.contains("# sxmc:end cli-ai:openai-codex\nUSER = 'outside'\n"));
        let expected = existing.replace("command = 'old'", "command = 'new'\nargs = ['serve']");
        let actual = toml::from_str(&updated).unwrap();
        assert!(same_toml_table(
            &toml::from_str(&expected).unwrap(),
            &actual
        ));
    }

    #[test]
    fn codex_writer_merges_the_snapshot_committed_before_its_lock_is_acquired() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let user = "model = 'local-fixture'\n";
        fs::write(&path, user).unwrap();
        let incumbent_lock = lock_codex_config(&path).unwrap();
        let incumbent = "[mcp_servers.first]\ncommand = 'first'\nargs = ['one']\n";
        let next = "[mcp_servers.second]\ncommand = 'second'\nargs = ['two']\n";
        let markers = toml_block_markers_for_scope("openai-codex");
        let committed = format!("{user}{}\n{incumbent}{}\n", markers.0, markers.1);
        let artifact = GeneratedArtifact {
            label: "OpenAI/Codex config".into(),
            target_path: path.clone(),
            content: next.into(),
            apply_strategy: ApplyStrategy::TomlManagedBlock,
            audience: ArtifactAudience::Client(AiClientProfile::OpenaiCodex),
            sidecar_scope: "openai-codex".into(),
        };
        update_codex_config(&artifact, false, |target| {
            let mut replacement = tempfile::NamedTempFile::new_in(temp.path()).unwrap();
            replacement.write_all(committed.as_bytes()).unwrap();
            replacement.persist(target).unwrap();
            drop(incumbent_lock);
            lock_codex_config(target)
        })
        .unwrap();
        let actual: toml::Table = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let expected: toml::Table = toml::from_str(&format!("{user}{incumbent}{next}")).unwrap();
        assert_eq!(actual, expected);
    }
}
