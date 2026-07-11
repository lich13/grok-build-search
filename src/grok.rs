use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use semver::Version;
use tempfile::TempDir;
use tokio::{
    process::Command,
    sync::{OnceCell, Semaphore},
    time::timeout,
};

use crate::model::parse_grok_fetch_json;
use crate::{ErrorCode, ResponseFormat, ToolError, ToolResponse, WebSearchInput, parse_grok_json};

const MINIMUM_GROK_VERSION: Version = Version::new(0, 2, 93);
const NEXT_UNSUPPORTED_GROK_VERSION: Version = Version::new(0, 3, 0);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_MAX_CONCURRENCY: usize = 2;
const MAX_STDERR_CHARS: usize = 2_000;
const DENY_RULES: &[&str] = &[
    "Bash(*)",
    "Read(*)",
    "Write(*)",
    "Edit(*)",
    "Glob(*)",
    "Grep(*)",
    "NotebookEdit(*)",
];

#[derive(Debug, Clone)]
pub struct GrokLocator {
    explicit: Option<PathBuf>,
    path: Option<OsString>,
    home: Option<PathBuf>,
}

impl GrokLocator {
    pub fn new(explicit: Option<PathBuf>, path: Option<OsString>, home: Option<PathBuf>) -> Self {
        Self {
            explicit,
            path,
            home,
        }
    }

    pub fn from_environment() -> Self {
        Self::new(
            std::env::var_os("GROK_BIN").map(PathBuf::from),
            std::env::var_os("PATH"),
            std::env::var_os("HOME").map(PathBuf::from),
        )
    }

    pub fn locate(&self) -> Result<PathBuf, ToolError> {
        if let Some(explicit) = self.explicit.as_ref().filter(|path| is_executable(path)) {
            return Ok(explicit.clone());
        }
        if let Some(path) = &self.path {
            for directory in std::env::split_paths(path) {
                let candidate = directory.join("grok");
                if is_executable(&candidate) {
                    return Ok(candidate);
                }
            }
        }
        if let Some(home) = &self.home {
            for candidate in [home.join(".local/bin/grok"), home.join(".grok/bin/grok")] {
                if is_executable(&candidate) {
                    return Ok(candidate);
                }
            }
        }
        Err(ToolError::new(
            ErrorCode::GrokNotFound,
            "could not find an executable Grok CLI via GROK_BIN, PATH, ~/.local/bin/grok, or ~/.grok/bin/grok",
        ))
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug, Clone)]
pub struct GrokConfig {
    binary: PathBuf,
    timeout: Duration,
    max_concurrency: usize,
    environment: BTreeMap<OsString, OsString>,
}

impl GrokConfig {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            timeout: DEFAULT_TIMEOUT,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            environment: BTreeMap::new(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = max_concurrency;
        self
    }

    pub fn with_environment(mut self, environment: BTreeMap<OsString, OsString>) -> Self {
        self.environment = environment;
        self
    }
}

#[derive(Debug, Clone)]
pub struct GrokClient {
    config: GrokConfig,
    semaphore: Arc<Semaphore>,
    version: Arc<OnceCell<Version>>,
}

impl GrokClient {
    pub fn new(config: GrokConfig) -> Result<Self, ToolError> {
        if config.max_concurrency == 0 {
            return Err(ToolError::new(
                ErrorCode::GrokExitFailed,
                "Grok process concurrency must be at least one",
            ));
        }
        if config.timeout.is_zero() {
            return Err(ToolError::new(
                ErrorCode::GrokTimeout,
                "Grok process timeout must be greater than zero",
            ));
        }
        if !is_executable(&config.binary) {
            return Err(ToolError::new(
                ErrorCode::GrokNotFound,
                "configured Grok binary is missing or not executable",
            ));
        }
        Ok(Self {
            semaphore: Arc::new(Semaphore::new(config.max_concurrency)),
            version: Arc::new(OnceCell::new()),
            config,
        })
    }

    pub async fn probe_version(&self) -> Result<Version, ToolError> {
        self.version
            .get_or_try_init(|| self.detect_version())
            .await
            .cloned()
    }

    async fn detect_version(&self) -> Result<Version, ToolError> {
        let _permit = self.acquire_permit().await?;
        let mut command = self.base_command();
        command.arg("--version");
        let output = self.run_command(&mut command).await?;
        if !output.status.success() {
            return Err(exit_error(&output.stderr, None));
        }
        let stdout = String::from_utf8(output.stdout).map_err(|_| {
            ToolError::new(
                ErrorCode::GrokUnsupportedVersion,
                "Grok --version output is not valid UTF-8",
            )
        })?;
        let version_text = stdout.split_whitespace().nth(1).ok_or_else(|| {
            ToolError::new(
                ErrorCode::GrokUnsupportedVersion,
                "could not identify a semantic version in Grok --version output",
            )
        })?;
        let version = Version::parse(version_text).map_err(|error| {
            ToolError::new(
                ErrorCode::GrokUnsupportedVersion,
                format!("invalid Grok semantic version: {error}"),
            )
        })?;
        if !(MINIMUM_GROK_VERSION..NEXT_UNSUPPORTED_GROK_VERSION).contains(&version) {
            return Err(ToolError::new(
                ErrorCode::GrokUnsupportedVersion,
                format!(
                    "unsupported Grok version {version}; expected >= {MINIMUM_GROK_VERSION} and < {NEXT_UNSUPPORTED_GROK_VERSION}"
                ),
            ));
        }
        Ok(version)
    }

    pub async fn search(
        &self,
        query: &str,
        response_format: ResponseFormat,
    ) -> Result<ToolResponse, ToolError> {
        self.probe_version().await?;
        let validated = WebSearchInput {
            query: query.to_string(),
            response_format: Some(response_format),
        }
        .validate()?;
        let prompt = search_prompt(&validated.query, validated.response_format);
        let stdout = self.run_prompt(&prompt).await?;
        parse_grok_json(&stdout, validated.response_format)
    }

    pub async fn fetch(
        &self,
        url: &str,
        instructions: Option<&str>,
        max_chars: usize,
    ) -> Result<ToolResponse, ToolError> {
        self.probe_version().await?;
        let prompt = fetch_prompt(url, instructions, max_chars);
        let stdout = self.run_prompt(&prompt).await?;
        parse_grok_fetch_json(&stdout, max_chars, url)
    }

    async fn run_prompt(&self, prompt: &str) -> Result<String, ToolError> {
        let _permit = self.acquire_permit().await?;
        let workspace = tempfile::Builder::new()
            .prefix("grok-build-search-")
            .tempdir()
            .map_err(|error| {
                ToolError::new(
                    ErrorCode::GrokExitFailed,
                    format!("could not create isolated Grok working directory: {error}"),
                )
            })?;
        let mut prompt_file = tempfile::Builder::new()
            .prefix("prompt-")
            .tempfile_in(workspace.path())
            .map_err(|error| {
                ToolError::new(
                    ErrorCode::GrokExitFailed,
                    format!("could not create Grok prompt file: {error}"),
                )
            })?;
        set_prompt_permissions(prompt_file.path())?;
        prompt_file.write_all(prompt.as_bytes()).map_err(|error| {
            ToolError::new(
                ErrorCode::GrokExitFailed,
                format!("could not write Grok prompt file: {error}"),
            )
        })?;
        prompt_file.flush().map_err(|error| {
            ToolError::new(
                ErrorCode::GrokExitFailed,
                format!("could not flush Grok prompt file: {error}"),
            )
        })?;

        let mut command = self.base_command();
        add_guarded_arguments(&mut command);
        command
            .arg("--prompt-file")
            .arg(prompt_file.path())
            .current_dir(workspace.path())
            .env("GROK_WEB_FETCH", "1");
        let output = self.run_command(&mut command).await?;
        if !output.status.success() {
            return Err(exit_error(&output.stderr, Some(workspace.path())));
        }
        if !output.stderr.is_empty() {
            tracing::warn!(
                grok_stderr = %sanitize_stderr(&output.stderr, &workspace),
                "Grok completed with stderr output"
            );
        }
        String::from_utf8(output.stdout)
            .map_err(|_| ToolError::new(ErrorCode::BadGrokJson, "Grok stdout is not valid UTF-8"))
    }

    fn base_command(&self) -> Command {
        let mut command = Command::new(&self.config.binary);
        command
            .envs(&self.config.environment)
            .env_remove("XAI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("GROK_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    async fn run_command(&self, command: &mut Command) -> Result<std::process::Output, ToolError> {
        match timeout(self.config.timeout, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Err(ToolError::new(
                ErrorCode::GrokNotFound,
                "Grok binary could not be started",
            )),
            Ok(Err(error)) => Err(ToolError::new(
                ErrorCode::GrokExitFailed,
                format!("could not start Grok process: {error}"),
            )),
            Err(_) => Err(ToolError::new(
                ErrorCode::GrokTimeout,
                format!(
                    "Grok process exceeded the {} second timeout",
                    self.config.timeout.as_secs_f64()
                ),
            )),
        }
    }

    async fn acquire_permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, ToolError> {
        self.semaphore.acquire().await.map_err(|_| {
            ToolError::new(
                ErrorCode::GrokExitFailed,
                "Grok process limiter is unavailable",
            )
        })
    }
}

fn add_guarded_arguments(command: &mut Command) {
    command
        .arg("--no-plan")
        .arg("--no-subagents")
        .arg("--no-memory")
        .arg("--no-auto-update")
        .arg("--verbatim")
        .arg("--output-format")
        .arg("json")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--max-turns")
        .arg("8");
    for rule in DENY_RULES {
        command.arg("--deny").arg(rule);
    }
}

fn search_prompt(query: &str, response_format: ResponseFormat) -> String {
    let detail = match response_format {
        ResponseFormat::Concise => "Answer concisely.",
        ResponseFormat::Detailed => "Give a detailed answer.",
    };
    format!(
        "Search the public web for the user query below. {detail} Ground every factual claim in public HTTP(S) sources. Include exact source URLs in the answer. Do not describe internal tools or reasoning.\n\nUser query:\n{query}"
    )
}

fn fetch_prompt(url: &str, instructions: Option<&str>, max_chars: usize) -> String {
    let instructions = instructions.unwrap_or("Extract the page's main factual content.");
    format!(
        "Fetch the exact public URL below with the web fetch tool. Follow the instructions and return no more than {max_chars} characters. Include the exact URL as a source. Do not describe internal tools or reasoning.\n\nURL:\n{url}\n\nInstructions:\n{instructions}"
    )
}

fn set_prompt_permissions(path: &Path) -> Result<(), ToolError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            ToolError::new(
                ErrorCode::GrokExitFailed,
                format!("could not secure Grok prompt file: {error}"),
            )
        })?;
    }
    Ok(())
}

fn exit_error(stderr: &[u8], redacted_path: Option<&Path>) -> ToolError {
    let mut detail = sanitize_bytes(stderr);
    if let Some(path) = redacted_path {
        detail = detail.replace(path.to_string_lossy().as_ref(), "<temp>");
    }
    let message = if detail.is_empty() {
        "Grok process exited unsuccessfully".to_string()
    } else {
        format!("Grok process exited unsuccessfully: {detail}")
    };
    ToolError::new(ErrorCode::GrokExitFailed, message)
}

fn sanitize_stderr(stderr: &[u8], workspace: &TempDir) -> String {
    let sanitized = sanitize_bytes(stderr);
    sanitized.replace(&workspace.path().to_string_lossy().to_string(), "<temp>")
}

fn sanitize_bytes(input: &[u8]) -> String {
    let text = String::from_utf8_lossy(input);
    let mut without_ansi = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' && characters.peek() == Some(&'[') {
            characters.next();
            for sequence_character in characters.by_ref() {
                if ('@'..='~').contains(&sequence_character) {
                    break;
                }
            }
            continue;
        }
        without_ansi.push(character);
    }
    without_ansi
        .split_whitespace()
        .map(|token| {
            let lowercase = token.to_ascii_lowercase();
            if lowercase.starts_with("sk-")
                || lowercase.starts_with("xai-")
                || lowercase.contains("token=")
                || lowercase.contains("api_key=")
            {
                "<redacted>"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_STDERR_CHARS)
        .collect()
}
