use std::path::PathBuf;

use clap::ValueEnum;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const PROFILE_SCHEMA: &str = "sxmc_cli_surface_profile_v1";
pub const CLI_AI_HOSTS_LAST_VERIFIED: &str = "2026-03-21";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliSurfaceProfile {
    #[serde(default)]
    pub profile_schema: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub source: ProfileSource,
    #[serde(default)]
    pub subcommands: Vec<ProfileSubcommand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcommand_profiles: Vec<CliSurfaceProfile>,
    #[serde(default)]
    pub options: Vec<ProfileOption>,
    #[serde(default)]
    pub positionals: Vec<ProfilePositional>,
    #[serde(default)]
    pub examples: Vec<ProfileExample>,
    #[serde(default)]
    pub auth: Vec<AuthRequirement>,
    #[serde(default)]
    pub environment: Vec<EnvironmentRequirement>,
    #[serde(default)]
    pub output_behavior: OutputBehavior,
    #[serde(default)]
    pub workflows: Vec<Workflow>,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactive_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_interactive_alternatives: Vec<String>,
    #[serde(default)]
    pub confidence_notes: Vec<ConfidenceNote>,
    #[serde(default)]
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileSource {
    pub kind: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSubcommand {
    pub name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactive_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_interactive_alternatives: Vec<String>,
    #[serde(default)]
    pub confidence: ConfidenceLevel,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OptionRequirementSource {
    UsageCompulsory,
    UsageOptional,
    UsageAmbiguous,
    OptionMarker,
}

#[derive(Debug, Clone)]
pub struct ProfileOption {
    pub name: String,
    pub short: Option<String>,
    pub value_name: Option<String>,
    pub required: bool,
    pub required_known: bool,
    pub required_source: Option<OptionRequirementSource>,
    pub summary: Option<String>,
    pub confidence: ConfidenceLevel,
}

impl ProfileOption {
    /// Returns `Some(true|false)` only when requiredness is backed by usage/marker
    /// evidence or an explicit legacy boolean; `None` when omitted or ambiguous.
    pub fn known_required(&self) -> Option<bool> {
        if self.required_source == Some(OptionRequirementSource::UsageAmbiguous) {
            return None;
        }
        if self.required_known || self.required || self.required_source.is_some() {
            Some(self.required)
        } else {
            None
        }
    }
}

impl Serialize for ProfileOption {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let known_req = self.known_required();
        let mut field_count = 2; // name + confidence
        if self.short.is_some() {
            field_count += 1;
        }
        if self.value_name.is_some() {
            field_count += 1;
        }
        if known_req.is_some() {
            field_count += 1;
        }
        if self.required_source.is_some() {
            field_count += 1;
        }
        if self.summary.is_some() {
            field_count += 1;
        }
        let mut state = serializer.serialize_struct("ProfileOption", field_count)?;
        state.serialize_field("name", &self.name)?;
        if let Some(short) = &self.short {
            state.serialize_field("short", short)?;
        }
        if let Some(value_name) = &self.value_name {
            state.serialize_field("value_name", value_name)?;
        }
        if let Some(req) = known_req {
            state.serialize_field("required", &req)?;
        }
        if let Some(source) = &self.required_source {
            state.serialize_field("required_source", source)?;
        }
        if let Some(summary) = &self.summary {
            state.serialize_field("summary", summary)?;
        }
        state.serialize_field("confidence", &self.confidence)?;
        state.end()
    }
}

#[derive(Deserialize)]
struct RawProfileOption {
    name: String,
    #[serde(default)]
    short: Option<String>,
    #[serde(default)]
    value_name: Option<String>,
    #[serde(default)]
    required: Option<bool>,
    #[serde(default)]
    required_source: Option<OptionRequirementSource>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    confidence: ConfidenceLevel,
}

impl<'de> Deserialize<'de> for ProfileOption {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawProfileOption::deserialize(deserializer)?;
        let (required, required_known) = match raw.required_source {
            Some(OptionRequirementSource::UsageAmbiguous) => (false, false),
            _ => match raw.required {
                Some(req) => (req, true),
                None => match raw.required_source {
                    Some(
                        OptionRequirementSource::UsageCompulsory
                        | OptionRequirementSource::OptionMarker,
                    ) => (true, true),
                    Some(OptionRequirementSource::UsageOptional) => (false, true),
                    Some(OptionRequirementSource::UsageAmbiguous) | None => (false, false),
                },
            },
        };
        Ok(Self {
            name: raw.name,
            short: raw.short,
            value_name: raw.value_name,
            required,
            required_known,
            required_source: raw.required_source,
            summary: raw.summary,
            confidence: raw.confidence,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilePositional {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub confidence: ConfidenceLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileExample {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub confidence: ConfidenceLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthRequirement {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentRequirement {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputBehavior {
    #[serde(default)]
    pub stdout_style: String,
    #[serde(default)]
    pub stderr_usage: String,
    #[serde(default)]
    pub machine_friendly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub confidence: ConfidenceLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceNote {
    #[serde(default)]
    pub level: ConfidenceLevel,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct ProfileQualityReport {
    pub ready_for_agent_docs: bool,
    pub score: u8,
    pub level: String,
    pub reasons: Vec<String>,
}

impl CliSurfaceProfile {
    pub fn quality_report(&self) -> ProfileQualityReport {
        let mut reasons = Vec::new();
        let mut score: i32 = 100;
        let generic_summary = self.summary.trim()
            == format!("{} command-line interface", self.command)
            || self.summary.trim().eq_ignore_ascii_case(&self.command)
            || self.summary.trim().len() < 24;
        if generic_summary {
            score -= 25;
            reasons.push(
                "Summary stayed generic, so generated startup docs may be less useful than a hand-written snippet."
                    .into(),
            );
        }
        if self.examples.is_empty() {
            score -= 15;
            reasons.push(
                "No usage examples were detected, so generated guidance may not show the best first command."
                    .into(),
            );
        }
        if self.subcommands.is_empty() && self.options.len() < 2 {
            score -= 25;
            reasons.push(
                "The inspected CLI surface is sparse; sxmc could not confidently extract subcommands or enough options."
                    .into(),
            );
        }
        if self.subcommand_profiles.is_empty() && self.subcommands.len() >= 3 {
            score -= 10;
            reasons.push(
                "Only top-level subcommands were captured; deeper nested help may still need a higher inspection depth."
                    .into(),
            );
        }
        if self.output_behavior.machine_friendly {
            score += 5;
        }
        if self.options.len() >= 8 {
            score += 5;
        }

        let rich_surface = !self.examples.is_empty()
            || !self.subcommand_profiles.is_empty()
            || self.subcommands.len() >= 3
            || self.options.len() >= 5;

        let score = score.clamp(0, 100) as u8;
        let level = if score >= 80 {
            "high"
        } else if score >= 55 {
            "medium"
        } else {
            "low"
        };

        ProfileQualityReport {
            ready_for_agent_docs: rich_surface
                && !(generic_summary && self.subcommands.is_empty() && self.options.len() < 3),
            score,
            level: level.into(),
            reasons,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Provenance {
    #[serde(default)]
    pub generated_by: String,
    #[serde(default)]
    pub generator_version: String,
    #[serde(default)]
    pub source_kind: String,
    #[serde(default)]
    pub source_identifier: String,
    #[serde(default)]
    pub profile_schema: String,
    #[serde(default)]
    pub generation_depth: u32,
    #[serde(default)]
    pub generated_at: String,
}

#[derive(Debug, Copy, Clone, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceLevel {
    High,
    #[default]
    Medium,
    Low,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
pub enum AiClientProfile {
    ClaudeCode,
    Cursor,
    GeminiCli,
    GithubCopilot,
    ContinueDev,
    OpenCode,
    JetbrainsAiAssistant,
    Junie,
    Windsurf,
    OpenaiCodex,
    GenericStdioMcp,
    GenericHttpMcp,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
pub enum ArtifactMode {
    Preview,
    WriteSidecar,
    Patch,
    Apply,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
pub enum AiCoverage {
    Single,
    Full,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ArtifactAudience {
    Shared,
    Portable,
    Client(AiClientProfile),
}

#[derive(Debug, Clone)]
pub struct GeneratedArtifact {
    pub label: String,
    pub target_path: PathBuf,
    pub content: String,
    pub apply_strategy: ApplyStrategy,
    pub audience: ArtifactAudience,
    pub sidecar_scope: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ApplyStrategy {
    SidecarOnly,
    ManagedMarkdownBlock,
    JsonMcpConfig,
    TomlManagedBlock,
    DirectWrite,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ConfigShape {
    JsonMcpServers,
    JsonMcp,
    TomlMcpServers,
}

#[derive(Debug, Clone, Copy)]
pub struct HostProfileSpec {
    pub client: AiClientProfile,
    pub label: &'static str,
    pub sidecar_scope: &'static str,
    pub native_doc_target: Option<&'static str>,
    pub native_config_target: Option<&'static str>,
    pub config_shape: Option<ConfigShape>,
    pub official_reference_url: &'static str,
}

pub const AI_HOST_SPECS: &[HostProfileSpec] = &[
    HostProfileSpec {
        client: AiClientProfile::ClaudeCode,
        label: "Claude Code",
        sidecar_scope: "claude-code",
        native_doc_target: Some("CLAUDE.md"),
        native_config_target: Some(".sxmc/ai/claude-code-mcp.json"),
        config_shape: Some(ConfigShape::JsonMcpServers),
        official_reference_url: "https://docs.anthropic.com/en/docs/claude-code/memory",
    },
    HostProfileSpec {
        client: AiClientProfile::Cursor,
        label: "Cursor",
        sidecar_scope: "cursor",
        native_doc_target: Some(".cursor/rules/sxmc-cli-ai.md"),
        native_config_target: Some(".cursor/mcp.json"),
        config_shape: Some(ConfigShape::JsonMcpServers),
        official_reference_url: "https://docs.cursor.com/context/rules-for-ai",
    },
    HostProfileSpec {
        client: AiClientProfile::GeminiCli,
        label: "Gemini CLI",
        sidecar_scope: "gemini-cli",
        native_doc_target: Some("GEMINI.md"),
        native_config_target: Some(".gemini/settings.json"),
        config_shape: Some(ConfigShape::JsonMcpServers),
        official_reference_url: "https://geminicli.com/docs/cli/gemini-md/",
    },
    HostProfileSpec {
        client: AiClientProfile::GithubCopilot,
        label: "GitHub Copilot",
        sidecar_scope: "github-copilot",
        native_doc_target: Some(".github/copilot-instructions.md"),
        native_config_target: None,
        config_shape: None,
        official_reference_url: "https://docs.github.com/en/copilot/tutorials/customization-library/custom-instructions/your-first-custom-instructions",
    },
    HostProfileSpec {
        client: AiClientProfile::ContinueDev,
        label: "Continue",
        sidecar_scope: "continue",
        native_doc_target: Some(".continue/rules/sxmc-cli-ai.md"),
        native_config_target: None,
        config_shape: None,
        official_reference_url: "https://docs.continue.dev/customize/rules",
    },
    HostProfileSpec {
        client: AiClientProfile::OpenCode,
        label: "OpenCode",
        sidecar_scope: "opencode",
        native_doc_target: Some("AGENTS.md"),
        native_config_target: Some("opencode.json"),
        config_shape: Some(ConfigShape::JsonMcp),
        official_reference_url: "https://opencode.ai/docs/rules",
    },
    HostProfileSpec {
        client: AiClientProfile::JetbrainsAiAssistant,
        label: "JetBrains AI Assistant",
        sidecar_scope: "jetbrains-ai-assistant",
        native_doc_target: Some(".aiassistant/rules/sxmc-cli-ai.md"),
        native_config_target: None,
        config_shape: None,
        official_reference_url: "https://www.jetbrains.com/help/ai-assistant/configure-project-rules.html",
    },
    HostProfileSpec {
        client: AiClientProfile::Junie,
        label: "Junie",
        sidecar_scope: "junie",
        native_doc_target: Some(".junie/guidelines.md"),
        native_config_target: None,
        config_shape: None,
        official_reference_url: "https://www.jetbrains.com/help/junie/customize-guidelines.html",
    },
    HostProfileSpec {
        client: AiClientProfile::Windsurf,
        label: "Windsurf",
        sidecar_scope: "windsurf",
        native_doc_target: Some(".windsurf/rules/sxmc-cli-ai.md"),
        native_config_target: None,
        config_shape: None,
        official_reference_url: "https://docs.windsurf.com/windsurf/cascade/memories",
    },
    HostProfileSpec {
        client: AiClientProfile::OpenaiCodex,
        label: "OpenAI/Codex",
        sidecar_scope: "openai-codex",
        native_doc_target: Some("AGENTS.md"),
        native_config_target: Some(".codex/config.toml"),
        config_shape: Some(ConfigShape::TomlMcpServers),
        official_reference_url: "https://developers.openai.com/codex/cli/",
    },
    HostProfileSpec {
        client: AiClientProfile::GenericStdioMcp,
        label: "Generic stdio MCP",
        sidecar_scope: "generic-stdio-mcp",
        native_doc_target: Some("AGENTS.md"),
        native_config_target: Some(".sxmc/ai/generic-stdio-mcp.json"),
        config_shape: Some(ConfigShape::JsonMcpServers),
        official_reference_url: "https://modelcontextprotocol.io/docs/learn/architecture",
    },
    HostProfileSpec {
        client: AiClientProfile::GenericHttpMcp,
        label: "Generic HTTP MCP",
        sidecar_scope: "generic-http-mcp",
        native_doc_target: Some("AGENTS.md"),
        native_config_target: Some(".sxmc/ai/generic-http-mcp.json"),
        config_shape: Some(ConfigShape::JsonMcpServers),
        official_reference_url: "https://modelcontextprotocol.io/docs/learn/architecture",
    },
];

#[derive(Debug, Clone)]
pub struct WriteOutcome {
    pub label: String,
    pub path: PathBuf,
    pub mode: ArtifactMode,
    pub status: WriteStatus,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum WriteStatus {
    Created,
    Updated,
    Skipped,
    Removed,
}

pub fn host_profile_spec(client: AiClientProfile) -> &'static HostProfileSpec {
    AI_HOST_SPECS
        .iter()
        .find(|spec| spec.client == client)
        .expect("missing host profile spec")
}
