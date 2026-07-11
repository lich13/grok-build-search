use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use grok_build_search_mcp::{ErrorCode, GrokClient, GrokConfig, GrokLocator, ResponseFormat};
use serde_json::Value;
use tempfile::TempDir;

fn fake_grok() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-grok")
}

fn write_executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn client_with(
    mode: &str,
    timeout: Duration,
    max_concurrency: usize,
    extra_environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> GrokClient {
    let mut environment =
        BTreeMap::from([(OsString::from("FAKE_GROK_MODE"), OsString::from(mode))]);
    environment.extend(extra_environment);
    GrokClient::new(
        GrokConfig::new(fake_grok())
            .with_timeout(timeout)
            .with_max_concurrency(max_concurrency)
            .with_environment(environment),
    )
    .unwrap()
}

#[test]
fn locator_uses_explicit_binary_before_path_and_home() {
    let temp = TempDir::new().unwrap();
    let explicit = temp.path().join("explicit-grok");
    let path_binary = temp.path().join("path-bin/grok");
    let home_binary = temp.path().join("home/.local/bin/grok");
    for candidate in [&explicit, &path_binary, &home_binary] {
        write_executable(candidate);
    }
    let path = std::env::join_paths([path_binary.parent().unwrap()]).unwrap();

    let locator = GrokLocator::new(
        Some(explicit.clone()),
        Some(path),
        Some(temp.path().join("home")),
    );

    assert_eq!(locator.locate().unwrap(), explicit);
}

#[test]
fn locator_falls_back_from_path_to_both_home_locations() {
    let temp = TempDir::new().unwrap();
    let path_binary = temp.path().join("path-bin/grok");
    let local_binary = temp.path().join("home/.local/bin/grok");
    let grok_home_binary = temp.path().join("home/.grok/bin/grok");
    for candidate in [&path_binary, &local_binary, &grok_home_binary] {
        write_executable(candidate);
    }
    let path = std::env::join_paths([path_binary.parent().unwrap()]).unwrap();
    let home = temp.path().join("home");

    assert_eq!(
        GrokLocator::new(None, Some(path), Some(home.clone()))
            .locate()
            .unwrap(),
        path_binary
    );
    fs::remove_file(&path_binary).unwrap();
    assert_eq!(
        GrokLocator::new(None, None, Some(home.clone()))
            .locate()
            .unwrap(),
        local_binary
    );
    fs::remove_file(&local_binary).unwrap();
    assert_eq!(
        GrokLocator::new(None, None, Some(home)).locate().unwrap(),
        grok_home_binary
    );
}

#[test]
fn locator_returns_stable_not_found_error() {
    let error = GrokLocator::new(None, Some(OsString::new()), None)
        .locate()
        .expect_err("missing Grok must fail");

    assert_eq!(error.code, ErrorCode::GrokNotFound);
}

#[tokio::test]
async fn probe_accepts_supported_version_and_rejects_future_minor() {
    let supported = client_with("search-success", Duration::from_secs(1), 2, []);
    assert_eq!(
        supported.probe_version().await.unwrap().to_string(),
        "0.2.93"
    );

    let future = client_with(
        "search-success",
        Duration::from_secs(1),
        2,
        [(
            OsString::from("FAKE_GROK_VERSION"),
            OsString::from("grok 0.3.0 (future)"),
        )],
    );
    let error = future
        .probe_version()
        .await
        .expect_err("future minor must fail closed");
    assert_eq!(error.code, ErrorCode::GrokUnsupportedVersion);
}

#[tokio::test]
async fn search_rejects_unsupported_version_before_prompt_execution() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("must-not-exist.json");
    let client = client_with(
        "search-success",
        Duration::from_secs(1),
        2,
        [
            (
                OsString::from("FAKE_GROK_VERSION"),
                OsString::from("grok 0.3.0 (future)"),
            ),
            (
                OsString::from("FAKE_GROK_LOG"),
                log_path.clone().into_os_string(),
            ),
        ],
    );

    let error = client
        .search("must not run", ResponseFormat::Concise)
        .await
        .expect_err("unsupported version must fail before search");

    assert_eq!(error.code, ErrorCode::GrokUnsupportedVersion);
    assert!(!log_path.exists());
}

#[tokio::test]
async fn search_uses_isolated_prompt_file_and_guarded_arguments() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("invocation.json");
    let query = "sensitive query value must not appear in argv";
    let client = client_with(
        "stderr-warning",
        Duration::from_secs(2),
        2,
        [
            (
                OsString::from("FAKE_GROK_LOG"),
                log_path.clone().into_os_string(),
            ),
            (OsString::from("XAI_API_KEY"), OsString::from("xai-secret")),
            (
                OsString::from("OPENAI_API_KEY"),
                OsString::from("openai-secret"),
            ),
            (
                OsString::from("ANTHROPIC_API_KEY"),
                OsString::from("anthropic-secret"),
            ),
        ],
    );

    let output = client
        .search(query, ResponseFormat::Concise)
        .await
        .expect("stderr warning must not fail a successful process");

    assert!(output.ok);
    assert_eq!(output.sources[0].url, "https://example.com/warning");
    let invocation: Value = serde_json::from_slice(&fs::read(log_path).unwrap()).unwrap();
    let arguments: Vec<&str> = invocation["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();

    for required in [
        "--no-plan",
        "--no-subagents",
        "--no-memory",
        "--no-auto-update",
        "--verbatim",
        "--output-format",
        "--sandbox",
        "--prompt-file",
    ] {
        assert!(
            arguments.contains(&required),
            "missing {required}: {arguments:?}"
        );
    }
    assert!(!arguments.contains(&query));
    assert!(!arguments.contains(&"--tools"));
    assert!(!arguments.contains(&"--disallowed-tools"));
    assert_eq!(invocation["prompt_mode"], "0600");
    assert_eq!(invocation["grok_web_fetch"], "1");
    assert!(invocation["xai_api_key"].is_null());
    assert!(invocation["openai_api_key"].is_null());
    assert!(invocation["anthropic_api_key"].is_null());
    assert!(invocation["prompt"].as_str().unwrap().contains(query));
    assert_ne!(
        Path::new(invocation["cwd"].as_str().unwrap())
            .canonicalize()
            .ok(),
        Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().ok()
    );

    let deny_rules: Vec<&str> = arguments
        .windows(2)
        .filter(|window| window[0] == "--deny")
        .map(|window| window[1])
        .collect();
    assert_eq!(
        deny_rules,
        [
            "Bash(*)",
            "Read(*)",
            "Write(*)",
            "Edit(*)",
            "Glob(*)",
            "Grep(*)",
            "NotebookEdit(*)",
        ],
        "guardrails must use only rule prefixes accepted by Grok 0.2.93"
    );
}

#[tokio::test]
async fn exit_error_redacts_prompt_path_and_credentials() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("invocation.json");
    let client = client_with(
        "exit-failed",
        Duration::from_secs(1),
        2,
        [
            (
                OsString::from("FAKE_GROK_LOG"),
                log_path.clone().into_os_string(),
            ),
            (
                OsString::from("FAKE_GROK_ERROR_DETAIL"),
                OsString::from("1"),
            ),
        ],
    );

    let error = client
        .search("redaction test", ResponseFormat::Concise)
        .await
        .expect_err("fake process must fail");
    let invocation: Value = serde_json::from_slice(&fs::read(log_path).unwrap()).unwrap();
    let arguments = invocation["args"].as_array().unwrap();
    let prompt_index = arguments
        .iter()
        .position(|argument| argument == "--prompt-file")
        .unwrap();
    let prompt_path = arguments[prompt_index + 1].as_str().unwrap();

    assert_eq!(error.code, ErrorCode::GrokExitFailed);
    assert!(!error.message.contains(prompt_path));
    assert!(!error.message.contains("secret-value"));
    assert!(!error.message.contains("token="));
}

#[tokio::test]
async fn process_failures_map_to_stable_error_codes() {
    let cases = [
        ("bad-json", ErrorCode::BadGrokJson, Duration::from_secs(1)),
        (
            "exit-failed",
            ErrorCode::GrokExitFailed,
            Duration::from_secs(1),
        ),
        ("no-sources", ErrorCode::NoSources, Duration::from_secs(1)),
        ("sleep", ErrorCode::GrokTimeout, Duration::from_millis(20)),
    ];

    for (mode, expected, timeout) in cases {
        let client = client_with(
            mode,
            timeout,
            2,
            [(
                OsString::from("FAKE_GROK_SLEEP_SECONDS"),
                OsString::from("1"),
            )],
        );
        let error = client
            .search("test error mapping", ResponseFormat::Concise)
            .await
            .expect_err("mode must fail");
        assert_eq!(error.code, expected, "unexpected code for {mode}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn process_concurrency_is_limited_to_two() {
    let client = client_with(
        "sleep",
        Duration::from_secs(2),
        2,
        [(
            OsString::from("FAKE_GROK_SLEEP_SECONDS"),
            OsString::from("0.20"),
        )],
    );
    let started = Instant::now();

    let first = client.search("first", ResponseFormat::Concise);
    let second = client.search("second", ResponseFormat::Concise);
    let third = client.search("third", ResponseFormat::Concise);
    let (first, second, third) = tokio::join!(first, second, third);

    first.unwrap();
    second.unwrap();
    third.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(350),
        "limit was bypassed: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(900),
        "processes ran serially: {elapsed:?}"
    );
}
