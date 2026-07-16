use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use semver::Version;
use tokio::{
    process::Command,
    sync::{OnceCell, Semaphore},
    task::JoinHandle,
    time::timeout,
};

use crate::{ErrorCode, ResponseFormat, ToolError, ToolResponse, WebSearchInput, parse_grok_json};
use crate::{model::parse_grok_fetch_json, runtime::RuntimeManager};

const PROCESS_REAP_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_MAX_CONCURRENCY: usize = 2;
const MAX_STDERR_CHARS: usize = 2_000;
const ALLOWED_BUILTIN_TOOLS: &str = "web_search,web_fetch";
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
    timeout: Option<Duration>,
    max_concurrency: usize,
    environment: BTreeMap<OsString, OsString>,
    runtime_root: Option<PathBuf>,
}

impl GrokConfig {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            timeout: None,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            environment: BTreeMap::new(),
            runtime_root: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
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

    pub fn with_runtime_root(mut self, runtime_root: impl Into<PathBuf>) -> Self {
        self.runtime_root = Some(runtime_root.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct GrokClient {
    config: GrokConfig,
    semaphore: Arc<Semaphore>,
    version: Arc<OnceCell<CachedVersionProbe>>,
    runtime: RuntimeManager,
}

impl GrokClient {
    pub fn new(config: GrokConfig) -> Result<Self, ToolError> {
        if config.max_concurrency == 0 {
            return Err(ToolError::new(
                ErrorCode::GrokExitFailed,
                "Grok process concurrency must be at least one",
            ));
        }
        if config.timeout.is_some_and(|timeout| timeout.is_zero()) {
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
        let runtime = RuntimeManager::new(&config.environment, config.runtime_root.clone());
        Ok(Self {
            semaphore: Arc::new(Semaphore::new(config.max_concurrency)),
            version: Arc::new(OnceCell::new()),
            config,
            runtime,
        })
    }

    pub async fn probe_version(&self) -> Result<Version, ToolError> {
        self.version
            .get_or_try_init(|| self.detect_version())
            .await
            .map(|probe| probe.version.clone())
    }

    pub(crate) async fn probe_version_with_cleanup(&self) -> Result<(Version, bool), ToolError> {
        self.version
            .get_or_try_init(|| self.detect_version())
            .await
            .map(|probe| {
                (
                    probe.version.clone(),
                    probe.cleanup_deferred.swap(false, Ordering::AcqRel),
                )
            })
    }

    async fn detect_version(&self) -> Result<CachedVersionProbe, ToolError> {
        let _permit = self.acquire_permit().await?;
        let runtime = self.runtime.start().await?;
        let mut command = self.base_command();
        runtime.apply_environment(&mut command);
        command.arg("--version");
        let result = self.run_command(&mut command).await;
        let cleanup_deferred = runtime.finish();
        parse_version_output(result?).map(|version| CachedVersionProbe {
            version,
            cleanup_deferred: AtomicBool::new(cleanup_deferred),
        })
    }

    pub async fn search(
        &self,
        query: &str,
        response_format: ResponseFormat,
    ) -> Result<ToolResponse, ToolError> {
        let mut cleanup_deferred = self.cleanup_stale_runtimes().await;
        let (_, version_cleanup_deferred) = self.probe_version_with_cleanup().await?;
        cleanup_deferred |= version_cleanup_deferred;
        let validated = WebSearchInput {
            query: query.to_string(),
            response_format: Some(response_format),
        }
        .validate()?;
        let prompt = search_prompt(&validated.query, validated.response_format);
        let output = self.run_prompt(&prompt).await?;
        cleanup_deferred |= output.cleanup_deferred;
        let mut response = parse_grok_json(&output.stdout, validated.response_format)?;
        if cleanup_deferred {
            response.add_cleanup_deferred_warning();
        }
        Ok(response)
    }

    pub async fn fetch(
        &self,
        url: &str,
        instructions: Option<&str>,
        max_chars: usize,
    ) -> Result<ToolResponse, ToolError> {
        let mut cleanup_deferred = self.cleanup_stale_runtimes().await;
        let (_, version_cleanup_deferred) = self.probe_version_with_cleanup().await?;
        cleanup_deferred |= version_cleanup_deferred;
        let prompt = fetch_prompt(url, instructions, max_chars);
        let output = self.run_prompt(&prompt).await?;
        cleanup_deferred |= output.cleanup_deferred;
        let mut response = parse_grok_fetch_json(&output.stdout, max_chars, url)?;
        if cleanup_deferred {
            response.add_cleanup_deferred_warning();
        }
        Ok(response)
    }

    pub(crate) async fn cleanup_stale_runtimes(&self) -> bool {
        self.runtime.cleanup_stale().await
    }

    async fn run_prompt(&self, prompt: &str) -> Result<PromptOutput, ToolError> {
        let _permit = self.acquire_permit().await?;
        let runtime = self.runtime.start().await?;
        let runtime_path = runtime.path().to_path_buf();
        let mut prompt_file = tempfile::Builder::new()
            .prefix("prompt-")
            .tempfile_in(&runtime_path)
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
        runtime.apply_environment(&mut command);
        add_guarded_arguments(&mut command, runtime.reasoning_effort());
        command
            .arg("--prompt-file")
            .arg(prompt_file.path())
            .current_dir(&runtime_path)
            .env("GROK_WEB_FETCH", "1");
        let result = self.run_command(&mut command).await.and_then(|output| {
            if !output.status.success() {
                return Err(exit_error(&output.stderr, Some(&runtime_path)));
            }
            if !output.stderr.is_empty() {
                tracing::warn!(
                    grok_stderr = %sanitize_stderr(&output.stderr, &runtime_path),
                    "Grok completed with stderr output"
                );
            }
            String::from_utf8(output.stdout).map_err(|_| {
                ToolError::new(ErrorCode::BadGrokJson, "Grok stdout is not valid UTF-8")
            })
        });
        let cleanup_deferred = runtime.finish();
        match result {
            Ok(stdout) => Ok(PromptOutput {
                stdout,
                cleanup_deferred,
            }),
            Err(error) => Err(error),
        }
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
        #[cfg(unix)]
        command.process_group(0);
        command
    }

    async fn run_command(&self, command: &mut Command) -> Result<std::process::Output, ToolError> {
        let child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ToolError::new(ErrorCode::GrokNotFound, "Grok binary could not be started")
            } else {
                ToolError::new(
                    ErrorCode::GrokExitFailed,
                    format!("could not start Grok process: {error}"),
                )
            }
        })?;
        let pid = child.id();
        let mut output_task =
            OutputTaskGuard::new(tokio::spawn(async move { child.wait_with_output().await }));
        let mut process_group = ProcessGroupGuard::new(pid);
        let output = match self.config.timeout {
            Some(process_timeout) => timeout(process_timeout, output_task.task()).await,
            None => Ok(output_task.task().await),
        };
        match output {
            Ok(Ok(Ok(output))) => {
                process_group.terminate();
                Ok(output)
            }
            Ok(Ok(Err(error))) => {
                process_group.terminate();
                Err(ToolError::new(
                    ErrorCode::GrokExitFailed,
                    format!("could not wait for Grok process: {error}"),
                ))
            }
            Ok(Err(error)) => {
                process_group.terminate();
                Err(ToolError::new(
                    ErrorCode::GrokExitFailed,
                    format!("Grok process task failed: {error}"),
                ))
            }
            Err(_) => {
                process_group.terminate();
                match timeout(PROCESS_REAP_TIMEOUT, output_task.task()).await {
                    Ok(Ok(Ok(_))) => {}
                    Ok(Ok(Err(error))) => {
                        tracing::warn!(%error, "Could not reap the timed-out Grok process");
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "Timed-out Grok process task failed while reaping");
                    }
                    Err(_) => tracing::warn!(
                        "Timed-out Grok process did not finish within the bounded reaping window"
                    ),
                }
                Err(ToolError::new(
                    ErrorCode::GrokTimeout,
                    format!(
                        "Grok process exceeded the {} second timeout",
                        self.config
                            .timeout
                            .expect("elapsed result requires a configured timeout")
                            .as_secs_f64()
                    ),
                ))
            }
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

struct OutputTaskGuard {
    task: JoinHandle<io::Result<std::process::Output>>,
}

impl OutputTaskGuard {
    fn new(task: JoinHandle<io::Result<std::process::Output>>) -> Self {
        Self { task }
    }

    fn task(&mut self) -> &mut JoinHandle<io::Result<std::process::Output>> {
        &mut self.task
    }
}

impl Drop for OutputTaskGuard {
    fn drop(&mut self) {
        if !self.task.is_finished() {
            self.task.abort();
        }
    }
}

fn parse_version_output(output: std::process::Output) -> Result<Version, ToolError> {
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
    Ok(version)
}

#[derive(Debug)]
struct CachedVersionProbe {
    version: Version,
    cleanup_deferred: AtomicBool,
}

struct ProcessGroupGuard {
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    fn terminate(&mut self) {
        if let Some(pid) = self.pid.take() {
            terminate_process_group(pid);
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(unix)]
fn terminate_process_group(pid: u32) {
    let Some(pid) = rustix::process::Pid::from_raw(pid as i32) else {
        return;
    };
    if let Err(error) = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL)
        && error != rustix::io::Errno::SRCH
    {
        tracing::warn!(%error, "Could not terminate the Grok process group");
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_pid: u32) {}

fn add_guarded_arguments(command: &mut Command, reasoning_effort: Option<&str>) {
    command
        .arg("--no-plan")
        .arg("--no-subagents")
        .arg("--no-memory")
        .arg("--no-auto-update")
        .arg("--always-approve")
        .arg("--verbatim")
        .arg("--output-format")
        .arg("json")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--max-turns")
        .arg("8")
        .arg("--tools")
        .arg(ALLOWED_BUILTIN_TOOLS);
    if let Some(reasoning_effort) = reasoning_effort {
        command.arg("--reasoning-effort").arg(reasoning_effort);
    }
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

fn sanitize_stderr(stderr: &[u8], workspace: &Path) -> String {
    let sanitized = sanitize_bytes(stderr);
    sanitized.replace(&workspace.to_string_lossy().to_string(), "<temp>")
}

struct PromptOutput {
    stdout: String,
    cleanup_deferred: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_no_process_deadline() {
        let config = GrokConfig::new("grok");

        assert_eq!(config.timeout, None);
    }
}
