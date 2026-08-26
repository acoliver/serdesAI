use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

mod runtime;
pub use runtime::*;

const QUALIFIER: &str = "";
const ORGANIZATION: &str = "";
const APPLICATION: &str = "serdes-ai";

/// The directory settings live in, under the user's home.
const APP_DIR_NAME: &str = ".newcode";
/// The data subdirectory, used under XDG_DATA_HOME when that is set.
const APP_DATA_NAME: &str = "newcode";
const CONFIG_FILE_NAME: &str = "config.cfg";

/// Where settings lived when the application was called Code Puppy.
///
/// Kept so an existing install is carried over on first run rather than
/// silently reverting to defaults.
const LEGACY_DIR_NAME: &str = ".code_puppy";
const LEGACY_CONFIG_FILE_NAME: &str = "puppy.cfg";
const LEGACY_DATA_NAME: &str = "code_puppy";
const MODELS_FILE_NAME: &str = "models.json";
const EXTRA_MODELS_FILE_NAME: &str = "extra_models.json";
const AUTOSAVE_DIR_NAME: &str = "autosaves";
const COMMAND_HISTORY_FILE_NAME: &str = "command_history.txt";
const CONFIG_SECTION: &str = "newcode";
/// The section name used when the application was called Code Puppy.
const LEGACY_CONFIG_SECTION: &str = "puppy";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default)]
    pub colors: ColorsConfig,
    #[serde(default)]
    pub autosave: AutosaveConfig,

    #[serde(default)]
    pub compaction_strategy: CompactionStrategy,
    #[serde(default = "default_protected_token_count")]
    pub protected_token_count: usize,
    #[serde(default = "default_compaction_threshold")]
    pub compaction_threshold: f32,
    #[serde(default = "default_resume_message_count")]
    pub resume_message_count: usize,
    #[serde(default)]
    pub openai_reasoning_effort: ReasoningEffort,
    #[serde(default)]
    pub openai_verbosity: Verbosity,
    #[serde(default)]
    pub cancel_agent_key: CancelKey,
    #[serde(default)]
    pub subagent_verbose: bool,
    #[serde(default)]
    pub model_settings: HashMap<String, ModelSettings>,
    #[serde(default)]
    pub pinned_models: HashMap<String, String>,

    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub diff: DiffConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_keys: Option<HashMap<String, String>>,

    /// Per-provider endpoint overrides, for OpenAI-compatible servers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_urls: Option<HashMap<String, String>>,

    /// One model per gate verifier, for workflow mode.
    ///
    /// The default names three models from three providers, which needs
    /// credentials for all three. Anyone running against a single endpoint has
    /// to be able to say which models to use instead, or workflow mode is
    /// simply unavailable to them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_verifier_models: Option<Vec<String>>,

    /// How many times the gate may send work back before giving up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_max_rounds: Option<u32>,

    /// Command the gate runs to gather build and test evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_test_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    #[serde(default = "default_true")]
    pub autosave_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_autosave_sessions: Option<i64>,
    #[serde(default)]
    pub universal_constructor_enabled: bool,
    #[serde(default = "default_true")]
    pub compact_diffs: bool,
    #[serde(default)]
    pub onboarding_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    #[default]
    Summarization,
    Truncation,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    #[default]
    Medium,
    Minimal,
    Low,
    High,
    Xhigh,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Verbosity {
    #[default]
    Medium,
    Low,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CancelKey {
    #[default]
    CtrlC,
    CtrlK,
    CtrlQ,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelSettings {
    pub temperature: Option<f32>,
    pub seed: Option<i32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_agent")]
    pub agent: String,
    #[serde(default)]
    pub subagent_verbose: bool,
    #[serde(default)]
    pub enable_pack_agents: bool,
    #[serde(default = "default_true")]
    pub enable_universal_constructor: bool,
    #[serde(default = "default_true")]
    pub enable_streaming: bool,
    #[serde(default = "default_renderer")]
    pub renderer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorsConfig {
    #[serde(default = "default_banner_thinking")]
    pub banner_thinking: String,
    #[serde(default = "default_banner_shell_command")]
    pub banner_shell_command: String,
    #[serde(default = "default_banner_edit_file")]
    pub banner_edit_file: String,
    #[serde(default = "default_banner_directory_listing")]
    pub banner_directory_listing: String,
    #[serde(default = "default_banner_grep")]
    pub banner_grep: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffConfig {
    #[serde(default = "default_true")]
    pub compact: bool,
    #[serde(default = "default_diff_add_style")]
    pub add_style: String,
    #[serde(default = "default_diff_remove_style")]
    pub remove_style: String,
    #[serde(default = "default_diff_context_style")]
    pub context_style: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutosaveConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    #[serde(default, alias = "current_session_id")]
    pub current_session: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        let general = GeneralConfig::default();
        Self {
            model: Some(general.model.clone()),
            agent: Some(general.agent.clone()),
            colors: ColorsConfig::default(),
            autosave: AutosaveConfig::default(),
            compaction_strategy: CompactionStrategy::default(),
            protected_token_count: default_protected_token_count(),
            compaction_threshold: default_compaction_threshold(),
            resume_message_count: default_resume_message_count(),
            openai_reasoning_effort: ReasoningEffort::default(),
            openai_verbosity: Verbosity::default(),
            cancel_agent_key: CancelKey::default(),
            subagent_verbose: false,
            model_settings: HashMap::new(),
            pinned_models: HashMap::new(),
            general,
            diff: DiffConfig::default(),
            api_keys: None,
            base_urls: None,
            gate_verifier_models: None,
            gate_max_rounds: None,
            gate_test_command: None,
            request_timeout_secs: None,
            temperature: None,
            max_tokens: None,
            autosave_enabled: true,
            max_autosave_sessions: Some(default_max_sessions() as i64),
            universal_constructor_enabled: false,
            compact_diffs: true,
            onboarding_complete: false,
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            model: default_model(),
            agent: default_agent(),
            subagent_verbose: false,
            enable_pack_agents: false,
            enable_universal_constructor: default_true(),
            enable_streaming: default_true(),
            renderer: default_renderer(),
        }
    }
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            banner_thinking: default_banner_thinking(),
            banner_shell_command: default_banner_shell_command(),
            banner_edit_file: default_banner_edit_file(),
            banner_directory_listing: default_banner_directory_listing(),
            banner_grep: default_banner_grep(),
        }
    }
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            compact: default_true(),
            add_style: default_diff_add_style(),
            remove_style: default_diff_remove_style(),
            context_style: default_diff_context_style(),
        }
    }
}

impl Default for AutosaveConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            max_sessions: default_max_sessions(),
            current_session: None,
        }
    }
}

impl Config {
    pub fn load() -> io::Result<Self> {
        ensure_config_exists()?;

        match fs::read_to_string(get_config_file()) {
            Ok(raw) => {
                // Fall back to the old section name so a settings file written
                // by an earlier version is still read rather than silently
                // replaced by defaults.
                let mut section = parse_ini_section(&raw, CONFIG_SECTION);
                if section.is_empty() {
                    section = parse_ini_section(&raw, LEGACY_CONFIG_SECTION);
                }
                let mut cfg = Self::default();
                apply_ini_values(&mut cfg, &section);
                cfg.sync_legacy_fields();
                Ok(cfg)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        ensure_base_dirs()?;

        let mut normalized = self.clone();
        normalized.sync_legacy_fields();

        let serialized = serialize_ini_config(&normalized)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        fs::write(get_config_file(), serialized)
    }

    pub fn ensure_exists() -> io::Result<()> {
        ensure_config_exists()
    }

    pub(crate) fn sync_legacy_fields(&mut self) {
        self.general.model = self
            .model
            .clone()
            .unwrap_or_else(default_model)
            .trim()
            .to_string();
        self.general.agent = self
            .agent
            .clone()
            .unwrap_or_else(default_agent)
            .trim()
            .to_string();
        self.general.subagent_verbose = self.subagent_verbose;

        self.autosave.enabled = self.autosave_enabled;
        if let Some(max) = self.max_autosave_sessions {
            self.autosave.max_sessions = max.max(1) as usize;
        } else {
            self.max_autosave_sessions = Some(self.autosave.max_sessions as i64);
        }
        self.diff.compact = self.compact_diffs;

        self.compaction_threshold = self.compaction_threshold.clamp(0.0, 1.0);
        self.protected_token_count = self.protected_token_count.max(1);
        self.resume_message_count = self.resume_message_count.max(1);
    }
}

pub fn get_config_dir() -> PathBuf {
    get_app_dir()
}

pub fn get_data_dir() -> PathBuf {
    get_app_data_dir()
}

pub fn get_cache_dir() -> PathBuf {
    project_dirs().cache_dir().to_path_buf()
}

pub fn get_state_dir() -> PathBuf {
    project_dirs()
        .state_dir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(get_data_dir)
}

pub fn get_config_file() -> PathBuf {
    get_config_dir().join(CONFIG_FILE_NAME)
}

pub fn get_models_file() -> PathBuf {
    get_data_dir().join(MODELS_FILE_NAME)
}

pub fn get_extra_models_file() -> PathBuf {
    get_data_dir().join(EXTRA_MODELS_FILE_NAME)
}

pub fn get_app_dir() -> PathBuf {
    user_home_dir().join(APP_DIR_NAME)
}

pub fn get_app_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(get_app_dir)
        .join(APP_DATA_NAME)
}

/// The directory settings lived in under the previous name.
fn legacy_app_dir() -> PathBuf {
    user_home_dir().join(LEGACY_DIR_NAME)
}

/// Carry an install forward from `~/.code_puppy` to `~/.newcode`.
///
/// Renaming the directory would otherwise look to an existing user like their
/// settings, pins, and saved endpoints had been wiped. Copies rather than
/// moves, so anything still reading the old location keeps working and a
/// downgrade is not destructive.
///
/// Runs once: the presence of the new directory means the move already
/// happened, or the user is new and has nothing to carry over.
pub fn migrate_legacy_config() -> std::io::Result<bool> {
    let (old, new) = (legacy_app_dir(), get_app_dir());

    if new.exists() || !old.is_dir() {
        return Ok(false);
    }

    copy_dir(&old, &new)?;

    // The configuration file was renamed along with the directory.
    let legacy_config = new.join(LEGACY_CONFIG_FILE_NAME);
    if legacy_config.is_file() && !new.join(CONFIG_FILE_NAME).exists() {
        std::fs::rename(&legacy_config, new.join(CONFIG_FILE_NAME))?;
    }

    Ok(true)
}

/// Carry the data directory forward when XDG_DATA_HOME puts it outside the
/// home directory, where copying `~/.code_puppy` would not reach it.
pub fn migrate_legacy_data_dir() -> std::io::Result<bool> {
    let Some(root) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) else {
        return Ok(false);
    };

    let (old, new) = (root.join(LEGACY_DATA_NAME), root.join(APP_DATA_NAME));
    if new.exists() || !old.is_dir() {
        return Ok(false);
    }

    copy_dir(&old, &new)?;
    Ok(true)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;

    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());

        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }

    Ok(())
}

pub fn get_autosave_dir() -> PathBuf {
    get_cache_dir().join(AUTOSAVE_DIR_NAME)
}

pub fn get_command_history_file() -> PathBuf {
    get_state_dir().join(COMMAND_HISTORY_FILE_NAME)
}

pub fn ensure_config_exists() -> io::Result<()> {
    ensure_base_dirs()?;
    let cfg_path = get_config_file();
    if !cfg_path.exists() {
        Config::default().save()?;
    }
    Ok(())
}

pub(crate) fn ensure_base_dirs() -> io::Result<()> {
    fs::create_dir_all(get_config_dir())?;
    fs::create_dir_all(get_data_dir())?;
    fs::create_dir_all(get_cache_dir())?;
    fs::create_dir_all(get_state_dir())?;
    fs::create_dir_all(get_autosave_dir())?;
    Ok(())
}

pub(crate) fn set_env_if_missing(name: &str, value: Option<&str>) {
    let already_set = std::env::var_os(name)
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    if already_set {
        return;
    }

    if let Some(v) = value {
        if !v.trim().is_empty() {
            unsafe { std::env::set_var(name, v) };
        }
    }
}

fn project_dirs() -> ProjectDirs {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .expect("Unable to resolve XDG project directories for serdes-ai")
}

fn user_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(get_data_dir)
}

fn parse_ini_section(raw: &str, section: &str) -> HashMap<String, String> {
    let mut current_section = String::new();
    let mut out = HashMap::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = trimmed[1..trimmed.len() - 1].trim().to_lowercase();
            continue;
        }

        if current_section != section {
            continue;
        }

        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };

        out.insert(key.trim().to_lowercase(), value.trim().to_string());
    }

    out
}

fn apply_ini_values(cfg: &mut Config, values: &HashMap<String, String>) {
    if let Some(v) = values.get("model") {
        cfg.model = Some(v.clone());
    }
    if let Some(v) = values.get("agent") {
        cfg.agent = Some(v.clone());
    }

    if let Some(v) = values.get("compaction_strategy") {
        cfg.compaction_strategy = parse_compaction_strategy(v);
    }
    if let Some(v) = values.get("protected_token_count") {
        cfg.protected_token_count = parse_usize(v, cfg.protected_token_count);
    }
    if let Some(v) = values.get("compaction_threshold") {
        cfg.compaction_threshold = parse_f32(v, cfg.compaction_threshold);
    }
    if let Some(v) = values.get("resume_message_count") {
        cfg.resume_message_count = parse_usize(v, cfg.resume_message_count);
    }
    if let Some(v) = values.get("openai_reasoning_effort") {
        cfg.openai_reasoning_effort = parse_reasoning_effort(v);
    }
    if let Some(v) = values.get("openai_verbosity") {
        cfg.openai_verbosity = parse_verbosity(v);
    }
    if let Some(v) = values.get("cancel_agent_key") {
        cfg.cancel_agent_key = parse_cancel_key(v);
    }

    if let Some(v) = values.get("subagent_verbose") {
        cfg.subagent_verbose = parse_bool(v, cfg.subagent_verbose);
    }
    if let Some(v) = values.get("request_timeout_secs") {
        cfg.request_timeout_secs =
            Some(parse_i64(v, cfg.request_timeout_secs.unwrap_or(120)).max(1));
    }
    if let Some(v) = values.get("temperature") {
        cfg.temperature = Some(parse_f64(v, cfg.temperature.unwrap_or(0.7)));
    }
    if let Some(v) = values.get("max_tokens") {
        cfg.max_tokens = Some(parse_i64(v, cfg.max_tokens.unwrap_or(0)).max(0));
    }
    if let Some(v) = values.get("autosave_enabled") {
        cfg.autosave_enabled = parse_bool(v, cfg.autosave_enabled);
    }
    if let Some(v) = values.get("max_autosave_sessions") {
        cfg.max_autosave_sessions =
            Some(parse_i64(v, cfg.max_autosave_sessions.unwrap_or(10)).max(1));
    }
    if let Some(v) = values.get("universal_constructor_enabled") {
        cfg.universal_constructor_enabled = parse_bool(v, cfg.universal_constructor_enabled);
    }
    if let Some(v) = values.get("compact_diffs") {
        cfg.compact_diffs = parse_bool(v, cfg.compact_diffs);
    }
    if let Some(v) = values.get("onboarding_complete") {
        cfg.onboarding_complete = parse_bool(v, cfg.onboarding_complete);
    }

    if let Some(v) = values.get("banner_thinking") {
        cfg.colors.banner_thinking = v.clone();
    }
    if let Some(v) = values.get("banner_shell_command") {
        cfg.colors.banner_shell_command = v.clone();
    }
    if let Some(v) = values.get("banner_edit_file") {
        cfg.colors.banner_edit_file = v.clone();
    }
    if let Some(v) = values.get("banner_directory_listing") {
        cfg.colors.banner_directory_listing = v.clone();
    }
    if let Some(v) = values.get("banner_grep") {
        cfg.colors.banner_grep = v.clone();
    }

    if let Some(v) = values.get("diff_add_style") {
        cfg.diff.add_style = v.clone();
    }
    if let Some(v) = values.get("diff_remove_style") {
        cfg.diff.remove_style = v.clone();
    }
    if let Some(v) = values.get("diff_context_style") {
        cfg.diff.context_style = v.clone();
    }

    if let Some(v) = values
        .get("current_session_id")
        .or_else(|| values.get("current_session"))
    {
        cfg.autosave.current_session = if v.trim().is_empty() {
            None
        } else {
            Some(v.clone())
        };
    }

    if let Some(v) = values.get("renderer") {
        cfg.general.renderer = v.clone();
    }
    if let Some(v) = values.get("enable_pack_agents") {
        cfg.general.enable_pack_agents = parse_bool(v, cfg.general.enable_pack_agents);
    }
    if let Some(v) = values.get("enable_universal_constructor") {
        cfg.general.enable_universal_constructor =
            parse_bool(v, cfg.general.enable_universal_constructor);
    }
    if let Some(v) = values.get("enable_streaming") {
        cfg.general.enable_streaming = parse_bool(v, cfg.general.enable_streaming);
    }

    if let Some(v) = values.get("base_urls") {
        cfg.base_urls = serde_json::from_str::<HashMap<String, String>>(v).ok();
    }

    if let Some(v) = values.get("gate_verifier_models") {
        // Accepts a JSON array or a plain comma-separated list, since this is
        // most often typed by hand.
        cfg.gate_verifier_models = serde_json::from_str::<Vec<String>>(v).ok().or_else(|| {
            let models: Vec<String> = v
                .split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
            (!models.is_empty()).then_some(models)
        });
    }

    if let Some(v) = values.get("gate_max_rounds") {
        cfg.gate_max_rounds = v.trim().parse::<u32>().ok();
    }

    if let Some(v) = values.get("gate_test_command") {
        let command = v.trim();
        cfg.gate_test_command = (!command.is_empty()).then(|| command.to_string());
    }

    if let Some(v) = values.get("api_keys") {
        cfg.api_keys = serde_json::from_str::<HashMap<String, String>>(v).ok();
    }
    if let Some(v) = values.get("model_settings") {
        cfg.model_settings =
            serde_json::from_str::<HashMap<String, ModelSettings>>(v).unwrap_or_default();
    }
    if let Some(v) = values.get("pinned_models") {
        cfg.pinned_models = serde_json::from_str::<HashMap<String, String>>(v).unwrap_or_default();
    }
}

fn serialize_ini_config(cfg: &Config) -> serde_json::Result<String> {
    let mut out = format!("[{CONFIG_SECTION}]\n");

    write_kv(
        &mut out,
        "model",
        cfg.model.as_deref().unwrap_or(&cfg.general.model),
    );
    write_kv(
        &mut out,
        "agent",
        cfg.agent.as_deref().unwrap_or(&cfg.general.agent),
    );

    write_kv(
        &mut out,
        "compaction_strategy",
        match cfg.compaction_strategy {
            CompactionStrategy::Summarization => "summarization",
            CompactionStrategy::Truncation => "truncation",
        },
    );
    write_kv(
        &mut out,
        "protected_token_count",
        &cfg.protected_token_count.to_string(),
    );
    write_kv(
        &mut out,
        "compaction_threshold",
        &cfg.compaction_threshold.to_string(),
    );
    write_kv(
        &mut out,
        "resume_message_count",
        &cfg.resume_message_count.to_string(),
    );
    write_kv(
        &mut out,
        "openai_reasoning_effort",
        match cfg.openai_reasoning_effort {
            ReasoningEffort::Minimal => "minimal",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Xhigh => "xhigh",
        },
    );
    write_kv(
        &mut out,
        "openai_verbosity",
        match cfg.openai_verbosity {
            Verbosity::Low => "low",
            Verbosity::Medium => "medium",
            Verbosity::High => "high",
        },
    );
    write_kv(
        &mut out,
        "cancel_agent_key",
        match cfg.cancel_agent_key {
            CancelKey::CtrlC => "ctrl_c",
            CancelKey::CtrlK => "ctrl_k",
            CancelKey::CtrlQ => "ctrl_q",
        },
    );

    write_kv(
        &mut out,
        "subagent_verbose",
        &cfg.subagent_verbose.to_string(),
    );
    write_kv(
        &mut out,
        "request_timeout_secs",
        &cfg.request_timeout_secs.unwrap_or(120).to_string(),
    );
    write_kv(
        &mut out,
        "temperature",
        &cfg.temperature.unwrap_or(0.7).to_string(),
    );
    write_kv(
        &mut out,
        "max_tokens",
        &cfg.max_tokens.unwrap_or(0).to_string(),
    );
    write_kv(
        &mut out,
        "autosave_enabled",
        &cfg.autosave_enabled.to_string(),
    );
    write_kv(
        &mut out,
        "max_autosave_sessions",
        &cfg.max_autosave_sessions
            .unwrap_or(default_max_sessions() as i64)
            .to_string(),
    );
    write_kv(
        &mut out,
        "universal_constructor_enabled",
        &cfg.universal_constructor_enabled.to_string(),
    );
    write_kv(&mut out, "compact_diffs", &cfg.compact_diffs.to_string());
    write_kv(
        &mut out,
        "onboarding_complete",
        &cfg.onboarding_complete.to_string(),
    );

    write_kv(&mut out, "banner_thinking", &cfg.colors.banner_thinking);
    write_kv(
        &mut out,
        "banner_shell_command",
        &cfg.colors.banner_shell_command,
    );
    write_kv(&mut out, "banner_edit_file", &cfg.colors.banner_edit_file);
    write_kv(
        &mut out,
        "banner_directory_listing",
        &cfg.colors.banner_directory_listing,
    );
    write_kv(&mut out, "banner_grep", &cfg.colors.banner_grep);

    write_kv(&mut out, "diff_add_style", &cfg.diff.add_style);
    write_kv(&mut out, "diff_remove_style", &cfg.diff.remove_style);
    write_kv(&mut out, "diff_context_style", &cfg.diff.context_style);

    if let Some(current_session) = &cfg.autosave.current_session {
        write_kv(&mut out, "current_session_id", current_session);
    }

    write_kv(&mut out, "renderer", &cfg.general.renderer);
    write_kv(
        &mut out,
        "enable_pack_agents",
        &cfg.general.enable_pack_agents.to_string(),
    );
    write_kv(
        &mut out,
        "enable_universal_constructor",
        &cfg.general.enable_universal_constructor.to_string(),
    );
    write_kv(
        &mut out,
        "enable_streaming",
        &cfg.general.enable_streaming.to_string(),
    );

    let api_keys = serde_json::to_string(&cfg.api_keys.clone().unwrap_or_default())?;
    let model_settings = serde_json::to_string(&cfg.model_settings)?;
    let pinned_models = serde_json::to_string(&cfg.pinned_models)?;
    write_kv(&mut out, "api_keys", &api_keys);

    let base_urls = serde_json::to_string(&cfg.base_urls.clone().unwrap_or_default())?;
    write_kv(&mut out, "base_urls", &base_urls);

    if let Some(models) = &cfg.gate_verifier_models {
        write_kv(
            &mut out,
            "gate_verifier_models",
            &serde_json::to_string(models)?,
        );
    }
    if let Some(rounds) = cfg.gate_max_rounds {
        write_kv(&mut out, "gate_max_rounds", &rounds.to_string());
    }
    if let Some(command) = &cfg.gate_test_command {
        write_kv(&mut out, "gate_test_command", command);
    }
    write_kv(&mut out, "model_settings", &model_settings);
    write_kv(&mut out, "pinned_models", &pinned_models);

    Ok(out)
}

fn write_kv(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(value.trim());
    out.push('\n');
}

fn parse_bool(value: &str, default: bool) -> bool {
    value.parse::<bool>().unwrap_or(default)
}

fn parse_usize(value: &str, default: usize) -> usize {
    value.parse::<usize>().unwrap_or(default)
}

fn parse_i64(value: &str, default: i64) -> i64 {
    value.parse::<i64>().unwrap_or(default)
}

fn parse_f32(value: &str, default: f32) -> f32 {
    value.parse::<f32>().unwrap_or(default)
}

fn parse_f64(value: &str, default: f64) -> f64 {
    value.parse::<f64>().unwrap_or(default)
}

fn parse_reasoning_effort(value: &str) -> ReasoningEffort {
    match value {
        "minimal" => ReasoningEffort::Minimal,
        "low" => ReasoningEffort::Low,
        "high" => ReasoningEffort::High,
        "xhigh" => ReasoningEffort::Xhigh,
        _ => ReasoningEffort::Medium,
    }
}

fn parse_verbosity(value: &str) -> Verbosity {
    match value {
        "low" => Verbosity::Low,
        "high" => Verbosity::High,
        _ => Verbosity::Medium,
    }
}

fn parse_compaction_strategy(value: &str) -> CompactionStrategy {
    match value {
        "truncation" => CompactionStrategy::Truncation,
        _ => CompactionStrategy::Summarization,
    }
}

fn parse_cancel_key(value: &str) -> CancelKey {
    match value {
        "ctrl_k" => CancelKey::CtrlK,
        "ctrl_q" => CancelKey::CtrlQ,
        _ => CancelKey::CtrlC,
    }
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_model() -> String {
    "gpt-4o".to_string()
}

pub(crate) fn default_agent() -> String {
    default_agent_name()
}

/// The agent used when none is chosen.
pub fn default_agent_name() -> String {
    DEFAULT_AGENT.to_string()
}

/// The built-in agent's name.
pub const DEFAULT_AGENT: &str = "newcode";

/// The name the built-in agent had when the application was called Code Puppy.
///
/// Still accepted, so a saved setting or a script naming it keeps working.
pub const LEGACY_AGENT: &str = "code-puppy";

/// Resolve an agent name, mapping the previous name onto the current one.
pub fn canonical_agent_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.eq_ignore_ascii_case(LEGACY_AGENT) {
        DEFAULT_AGENT.to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn default_renderer() -> String {
    "legacy".to_string()
}

pub(crate) fn default_banner_thinking() -> String {
    "bright_cyan".to_string()
}

pub(crate) fn default_banner_shell_command() -> String {
    "bright_green".to_string()
}

pub(crate) fn default_banner_edit_file() -> String {
    "bright_yellow".to_string()
}

pub(crate) fn default_banner_directory_listing() -> String {
    "bright_blue".to_string()
}

pub(crate) fn default_banner_grep() -> String {
    "bright_magenta".to_string()
}

pub(crate) fn default_diff_add_style() -> String {
    "green".to_string()
}

pub(crate) fn default_diff_remove_style() -> String {
    "red".to_string()
}

pub(crate) fn default_diff_context_style() -> String {
    "dim".to_string()
}

pub(crate) fn default_max_sessions() -> usize {
    10
}

pub(crate) fn default_protected_token_count() -> usize {
    10_000
}

pub(crate) fn default_compaction_threshold() -> f32 {
    0.7
}

pub(crate) fn default_resume_message_count() -> usize {
    10
}

pub fn set_oauth_token(provider: &str, token: &str) -> anyhow::Result<()> {
    let provider = provider.trim().to_lowercase();
    if provider.is_empty() {
        return Err(anyhow::anyhow!("oauth provider cannot be empty"));
    }

    let key_name = format!("{}_oauth_token", provider);
    set_api_key(&key_name, token);
    Ok(())
}

pub fn get_oauth_token(provider: &str) -> Option<String> {
    let provider = provider.trim().to_lowercase();
    if provider.is_empty() {
        return None;
    }

    let key_name = format!("{}_oauth_token", provider);
    get_api_key(&key_name)
}
