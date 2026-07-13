use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
    time::Instant,
};

use fs2::FileExt;
use grok_build_search_mcp::{ErrorCode, GrokClient, GrokConfig, GrokLocator, ResponseFormat};
use serde_json::Value;
use tempfile::TempDir;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

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

fn client_with_runtime(
    mode: &str,
    timeout: Duration,
    max_concurrency: usize,
    runtime_root: &Path,
    extra_environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> GrokClient {
    let mut environment =
        BTreeMap::from([(OsString::from("FAKE_GROK_MODE"), OsString::from(mode))]);
    environment.extend(extra_environment);
    GrokClient::new(
        GrokConfig::new(fake_grok())
            .with_timeout(timeout)
            .with_max_concurrency(max_concurrency)
            .with_runtime_root(runtime_root)
            .with_environment(environment),
    )
    .unwrap()
}

fn runtime_entries(runtime_root: &Path) -> Vec<PathBuf> {
    if !runtime_root.exists() {
        return Vec::new();
    }
    fs::read_dir(runtime_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("grok-build-search-runtime-"))
        })
        .collect()
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn process_exists(pid: u64) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn wait_until_process_exits(pid: u64) -> bool {
    for _ in 0..50 {
        if !process_exists(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

#[cfg(unix)]
fn terminate_test_process(pid: u64) {
    let _ = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
    let _ = wait_until_process_exits(pid);
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
    let supported = client_with("search-success", TEST_TIMEOUT, 2, []);
    assert_eq!(
        supported.probe_version().await.unwrap().to_string(),
        "0.2.93"
    );

    let future = client_with(
        "search-success",
        TEST_TIMEOUT,
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
        TEST_TIMEOUT,
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
        TEST_TIMEOUT,
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
async fn grok_state_and_configuration_are_isolated_from_the_real_home() {
    let temp = TempDir::new().unwrap();
    let real_home = temp.path().join("real-home");
    let real_grok_home = real_home.join(".grok");
    let runtime_root = temp.path().join("runtimes");
    let log_path = temp.path().join("invocation.json");
    fs::create_dir_all(real_grok_home.join("sessions/existing-session")).unwrap();
    fs::create_dir_all(real_grok_home.join("memory/existing-workspace")).unwrap();
    fs::write(real_grok_home.join("config.toml"), "model = \"grok-4.5\"\n").unwrap();
    fs::write(real_grok_home.join("models_cache.json"), "{}\n").unwrap();
    fs::write(real_grok_home.join("agent_id"), "agent-123\n").unwrap();
    fs::write(real_grok_home.join("auth.json"), "{}\n").unwrap();

    let client = client_with_runtime(
        "search-success",
        TEST_TIMEOUT,
        2,
        &runtime_root,
        [
            (OsString::from("HOME"), real_home.clone().into_os_string()),
            (
                OsString::from("FAKE_GROK_LOG"),
                log_path.clone().into_os_string(),
            ),
            (OsString::from("FAKE_GROK_WRITE_STATE"), OsString::from("1")),
        ],
    );

    let output = client
        .search("isolated state", ResponseFormat::Concise)
        .await
        .expect("isolated search should succeed");

    assert!(output.warnings.is_empty());
    let invocation: Value = serde_json::from_slice(&fs::read(log_path).unwrap()).unwrap();
    let isolated_home = PathBuf::from(invocation["grok_home"].as_str().unwrap());
    assert_eq!(invocation["home"], invocation["grok_home"]);
    assert!(isolated_home.starts_with(&runtime_root));
    assert_eq!(invocation["grok_home_mode"], "0700");
    assert_eq!(
        invocation["cwd"].as_str().unwrap(),
        invocation["grok_home_resolved"].as_str().unwrap()
    );
    assert_eq!(invocation["grok_storage_mode"], "local");
    assert_eq!(
        invocation["grok_auth_path"].as_str().unwrap(),
        real_grok_home.join("auth.json").to_string_lossy()
    );
    assert_eq!(invocation["grok_config"], "model = \"grok-4.5\"\n");
    assert!(real_grok_home.join("sessions/existing-session").is_dir());
    assert!(real_grok_home.join("memory/existing-workspace").is_dir());
    assert!(!real_grok_home.join("sessions/fake-session").exists());
    assert!(!real_grok_home.join("prompt_history.jsonl").exists());
    assert!(!real_grok_home.join("memory/fake-workspace").exists());
    assert!(!real_grok_home.join("logs").exists());
    assert!(runtime_entries(&runtime_root).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn default_runtime_root_is_private_and_does_not_scan_system_temp() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("invocation.json");
    let unrelated = tempfile::Builder::new()
        .prefix("grok-build-search-runtime-unrelated-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    fs::write(unrelated.path().join("must-survive"), "unrelated\n").unwrap();
    let client = client_with(
        "search-success",
        TEST_TIMEOUT,
        2,
        [
            (
                OsString::from("FAKE_GROK_LOG"),
                log_path.clone().into_os_string(),
            ),
            (OsString::from("FAKE_GROK_WRITE_STATE"), OsString::from("1")),
        ],
    );

    client
        .search("private runtime root", ResponseFormat::Concise)
        .await
        .unwrap();

    let invocation: Value = serde_json::from_slice(&fs::read(log_path).unwrap()).unwrap();
    let isolated_home = PathBuf::from(invocation["grok_home"].as_str().unwrap());
    assert_ne!(isolated_home.parent().unwrap(), std::env::temp_dir());
    assert_eq!(invocation["grok_home_parent_mode"], "0700");
    assert_eq!(invocation["grok_home_mode"], "0700");
    assert!(unrelated.path().join("must-survive").is_file());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_cleanup_lock_wait_is_bounded() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    fs::create_dir(&runtime_root).unwrap();
    set_mode(&runtime_root, 0o700);
    let lock_path = runtime_root.join(".grok-build-search-runtime-cleanup.lock");
    let lock = fs::File::create(&lock_path).unwrap();
    set_mode(&lock_path, 0o600);
    lock.lock_exclusive().unwrap();
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_secs(2));
        lock.unlock().unwrap();
    });
    let client = client_with_runtime("search-success", TEST_TIMEOUT, 2, &runtime_root, []);

    let started = Instant::now();
    let result = client
        .search("bounded global lock", ResponseFormat::Concise)
        .await;
    let elapsed = started.elapsed();
    releaser.join().unwrap();

    assert!(result.is_err(), "a contended runtime lock must fail closed");
    assert!(
        elapsed < Duration::from_millis(1500),
        "runtime lock blocked for {elapsed:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_global_cleanup_lock_is_rejected() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    fs::create_dir(&runtime_root).unwrap();
    set_mode(&runtime_root, 0o700);
    let target = temp.path().join("unrelated-lock-target");
    fs::write(&target, "unchanged\n").unwrap();
    symlink(
        &target,
        runtime_root.join(".grok-build-search-runtime-cleanup.lock"),
    )
    .unwrap();
    let client = client_with_runtime("search-success", TEST_TIMEOUT, 2, &runtime_root, []);

    let error = client
        .search("reject symlinked lock", ResponseFormat::Concise)
        .await
        .expect_err("symlinked lock must fail closed");

    assert_eq!(error.code, ErrorCode::GrokExitFailed);
    assert_eq!(fs::read_to_string(target).unwrap(), "unchanged\n");
}

#[tokio::test]
async fn version_probe_uses_an_isolated_runtime() {
    let temp = TempDir::new().unwrap();
    let real_home = temp.path().join("real-home");
    let real_grok_home = real_home.join(".grok");
    let runtime_root = temp.path().join("runtimes");
    let version_log = temp.path().join("version.json");
    fs::create_dir_all(real_grok_home.join("sessions/existing-session")).unwrap();
    fs::write(real_grok_home.join("auth.json"), "{}\n").unwrap();
    let client = client_with_runtime(
        "search-success",
        TEST_TIMEOUT,
        2,
        &runtime_root,
        [
            (OsString::from("HOME"), real_home.into_os_string()),
            (
                OsString::from("FAKE_GROK_VERSION_LOG"),
                version_log.clone().into_os_string(),
            ),
            (
                OsString::from("FAKE_GROK_WRITE_STATE_ON_VERSION"),
                OsString::from("1"),
            ),
        ],
    );

    assert_eq!(client.probe_version().await.unwrap().to_string(), "0.2.93");

    let invocation: Value = serde_json::from_slice(&fs::read(version_log).unwrap()).unwrap();
    let isolated_home = PathBuf::from(invocation["grok_home"].as_str().unwrap());
    assert_eq!(invocation["home"], invocation["grok_home"]);
    assert!(isolated_home.starts_with(&runtime_root));
    assert_eq!(invocation["grok_home_mode"], "0700");
    assert!(real_grok_home.join("sessions/existing-session").is_dir());
    assert!(!real_grok_home.join("sessions/fake-session").exists());
    assert!(runtime_entries(&runtime_root).is_empty());
}

#[tokio::test]
async fn relative_grok_home_and_auth_path_are_resolved_before_runtime_isolation() {
    let temp = TempDir::new().unwrap();
    let real_home = temp.path().join("real-home");
    let relative_grok_home = PathBuf::from("relative-grok-home");
    let relative_auth_path = PathBuf::from("relative-auth.json");
    let absolute_grok_home = std::env::current_dir().unwrap().join(&relative_grok_home);
    let absolute_auth_path = std::env::current_dir().unwrap().join(&relative_auth_path);
    let runtime_root = temp.path().join("runtimes");
    let log_path = temp.path().join("invocation.json");
    fs::create_dir_all(&absolute_grok_home).unwrap();
    fs::write(
        absolute_grok_home.join("config.toml"),
        "model = \"relative\"\n",
    )
    .unwrap();
    fs::write(&absolute_auth_path, "{}\n").unwrap();
    let client = client_with_runtime(
        "search-success",
        TEST_TIMEOUT,
        2,
        &runtime_root,
        [
            (OsString::from("HOME"), real_home.into_os_string()),
            (
                OsString::from("GROK_HOME"),
                relative_grok_home.clone().into_os_string(),
            ),
            (
                OsString::from("GROK_AUTH_PATH"),
                relative_auth_path.clone().into_os_string(),
            ),
            (
                OsString::from("FAKE_GROK_LOG"),
                log_path.clone().into_os_string(),
            ),
        ],
    );

    let result = client
        .search("relative config", ResponseFormat::Concise)
        .await;
    let _ = fs::remove_dir_all(&absolute_grok_home);
    let _ = fs::remove_file(&absolute_auth_path);
    let output = result.expect("relative Grok paths must resolve before isolation");
    assert!(output.warnings.is_empty());
    let invocation: Value = serde_json::from_slice(&fs::read(log_path).unwrap()).unwrap();
    assert_eq!(
        invocation["grok_auth_path"].as_str().unwrap(),
        absolute_auth_path.to_string_lossy()
    );
    assert_eq!(invocation["grok_config"], "model = \"relative\"\n");
}

#[tokio::test]
async fn runtime_is_removed_after_backend_failure_and_timeout() {
    let cases = [
        ("exit-failed", TEST_TIMEOUT, ErrorCode::GrokExitFailed),
        ("sleep", Duration::from_millis(20), ErrorCode::GrokTimeout),
    ];

    for (mode, timeout, expected) in cases {
        let temp = TempDir::new().unwrap();
        let runtime_root = temp.path().join("runtimes");
        let client = client_with_runtime(
            mode,
            timeout,
            2,
            &runtime_root,
            [
                (OsString::from("FAKE_GROK_WRITE_STATE"), OsString::from("1")),
                (
                    OsString::from("FAKE_GROK_SLEEP_SECONDS"),
                    OsString::from("1"),
                ),
            ],
        );

        let error = client
            .search("cleanup after failure", ResponseFormat::Concise)
            .await
            .expect_err("mode should fail");

        assert_eq!(error.code, expected, "unexpected error for {mode}");
        assert!(
            runtime_entries(&runtime_root).is_empty(),
            "runtime leaked after {mode}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_terminates_the_grok_process_group_before_runtime_cleanup() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    let pid_log = temp.path().join("pids.json");
    let client = client_with_runtime(
        "spawn-descendant",
        Duration::from_secs(1),
        2,
        &runtime_root,
        [(
            OsString::from("FAKE_GROK_PID_LOG"),
            pid_log.clone().into_os_string(),
        )],
    );

    let mut search = tokio::spawn(async move {
        client
            .search("terminate process group", ResponseFormat::Concise)
            .await
    });
    tokio::select! {
        result = &mut search => {
            panic!("search finished before fake Grok recorded its process IDs: {result:?}");
        }
        result = tokio::time::timeout(Duration::from_secs(2), async {
            while !pid_log.is_file() {
                tokio::task::yield_now().await;
            }
        }) => {
            result.expect("fake Grok process must record its process IDs before timing out");
        }
    }
    let error = search
        .await
        .unwrap()
        .expect_err("fake Grok process must time out");
    assert_eq!(error.code, ErrorCode::GrokTimeout);
    let pids: Value = serde_json::from_slice(&fs::read(pid_log).unwrap()).unwrap();
    let parent = pids["parent"].as_u64().unwrap();
    let child = pids["child"].as_u64().unwrap();
    let parent_exited = wait_until_process_exits(parent);
    let child_exited = wait_until_process_exits(child);
    assert!(parent_exited, "timed-out Grok parent process survived");
    assert!(child_exited, "timed-out Grok descendant process survived");
    assert!(runtime_entries(&runtime_root).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn successful_parent_terminates_remaining_process_group_members() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    let pid_log = temp.path().join("pids.json");
    let client = client_with_runtime(
        "success-with-descendant",
        TEST_TIMEOUT,
        2,
        &runtime_root,
        [(
            OsString::from("FAKE_GROK_PID_LOG"),
            pid_log.clone().into_os_string(),
        )],
    );

    let output = client
        .search("clean descendants after success", ResponseFormat::Concise)
        .await
        .unwrap();
    assert!(output.ok);
    let pids: Value = serde_json::from_slice(&fs::read(pid_log).unwrap()).unwrap();
    let child = pids["child"].as_u64().unwrap();
    let child_exited = wait_until_process_exits(child);
    if !child_exited {
        terminate_test_process(child);
    }

    assert!(child_exited, "successful Grok descendant process survived");
    assert!(runtime_entries(&runtime_root).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_reap_wait_is_bounded_when_an_escaped_child_holds_pipes() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    let pid_log = temp.path().join("pids.json");
    let client = client_with_runtime(
        "escape-process-group",
        Duration::from_secs(1),
        2,
        &runtime_root,
        [(
            OsString::from("FAKE_GROK_PID_LOG"),
            pid_log.clone().into_os_string(),
        )],
    );
    let mut search = tokio::spawn(async move {
        client
            .search("bound process reaping", ResponseFormat::Concise)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !pid_log.is_file() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("escaped child must record its process IDs");
    let pids: Value = serde_json::from_slice(&fs::read(&pid_log).unwrap()).unwrap();
    let child = pids["child"].as_u64().unwrap();

    let completion = tokio::time::timeout(Duration::from_secs(4), &mut search).await;
    terminate_test_process(child);
    let completed = match completion {
        Ok(completed) => completed,
        Err(_) => {
            search.abort();
            let _ = search.await;
            panic!("Grok timeout exceeded its bounded process-reaping window");
        }
    };
    let error = completed
        .unwrap()
        .expect_err("escaped child mode must time out");

    assert_eq!(error.code, ErrorCode::GrokTimeout);
    assert!(runtime_entries(&runtime_root).is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_running_search_terminates_the_grok_process_group() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    let pid_log = temp.path().join("pids.json");
    let client = client_with_runtime(
        "spawn-descendant",
        TEST_TIMEOUT,
        2,
        &runtime_root,
        [(
            OsString::from("FAKE_GROK_PID_LOG"),
            pid_log.clone().into_os_string(),
        )],
    );
    let task = tokio::spawn(async move {
        client
            .search("cancel process group", ResponseFormat::Concise)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !pid_log.is_file() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fake Grok process must record its process IDs");
    let pids: Value = serde_json::from_slice(&fs::read(&pid_log).unwrap()).unwrap();
    let parent = pids["parent"].as_u64().unwrap();
    let child = pids["child"].as_u64().unwrap();
    task.abort();
    let _ = task.await;

    let parent_exited = wait_until_process_exits(parent);
    let child_exited = wait_until_process_exits(child);
    assert!(parent_exited, "cancelled Grok parent process survived");
    assert!(child_exited, "cancelled Grok descendant process survived");
    assert!(runtime_entries(&runtime_root).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_do_not_remove_each_others_active_runtime() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    let client = client_with_runtime(
        "sleep",
        TEST_TIMEOUT,
        2,
        &runtime_root,
        [(
            OsString::from("FAKE_GROK_SLEEP_SECONDS"),
            OsString::from("0.30"),
        )],
    );
    let first_client = client.clone();
    let second_client = client.clone();
    let first = tokio::spawn(async move {
        first_client
            .search("first active runtime", ResponseFormat::Concise)
            .await
    });
    let second = tokio::spawn(async move {
        second_client
            .search("second active runtime", ResponseFormat::Concise)
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime_entries(&runtime_root).len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both runtimes should coexist while calls are active");

    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert!(runtime_entries(&runtime_root).is_empty());
}

#[tokio::test]
async fn abandoned_runtime_is_removed_before_the_next_call() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    #[cfg(unix)]
    {
        fs::create_dir(&runtime_root).unwrap();
        set_mode(&runtime_root, 0o700);
    }
    let stale = runtime_root.join("grok-build-search-runtime-abandoned");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join(".active.lock"), "").unwrap();
    #[cfg(unix)]
    set_mode(&stale.join(".active.lock"), 0o600);
    fs::write(stale.join("orphaned-session"), "state").unwrap();
    let client = client_with_runtime("search-success", TEST_TIMEOUT, 2, &runtime_root, []);

    client
        .search("trigger stale cleanup", ResponseFormat::Concise)
        .await
        .unwrap();

    assert!(!stale.exists());
    assert!(runtime_entries(&runtime_root).is_empty());
}

#[tokio::test]
async fn stale_cleanup_skips_a_runtime_with_an_active_lock() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    #[cfg(unix)]
    {
        fs::create_dir(&runtime_root).unwrap();
        set_mode(&runtime_root, 0o700);
    }
    let active = runtime_root.join("grok-build-search-runtime-active");
    fs::create_dir_all(&active).unwrap();
    let lock = fs::File::create(active.join(".active.lock")).unwrap();
    lock.lock_exclusive().unwrap();
    let client = client_with_runtime("search-success", TEST_TIMEOUT, 2, &runtime_root, []);

    client
        .search("preserve active runtime", ResponseFormat::Concise)
        .await
        .unwrap();

    assert!(active.is_dir());
    lock.unlock().unwrap();
    fs::remove_dir_all(active).unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_activity_lock_is_held_until_deletion_finishes() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    let done_file = temp.path().join("grok-done");
    let lock_was_acquired = Arc::new(AtomicBool::new(false));
    let watcher_flag = Arc::clone(&lock_was_acquired);
    let watcher_root = runtime_root.clone();
    let watcher_done = done_file.clone();
    let watcher = thread::spawn(move || {
        while !watcher_done.exists() {
            thread::yield_now();
        }
        loop {
            let Some(runtime) = runtime_entries(&watcher_root).into_iter().next() else {
                break;
            };
            match fs::File::open(runtime.join(".active.lock")) {
                Ok(lock) => match lock.try_lock_exclusive() {
                    Ok(()) => {
                        watcher_flag.store(true, Ordering::SeqCst);
                        let _ = lock.unlock();
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => break,
            }
            thread::yield_now();
        }
    });
    let client = client_with_runtime(
        "search-success",
        TEST_TIMEOUT,
        2,
        &runtime_root,
        [
            (
                OsString::from("FAKE_GROK_DONE_FILE"),
                done_file.into_os_string(),
            ),
            (
                OsString::from("FAKE_GROK_MANY_FILES"),
                OsString::from("5000"),
            ),
            (OsString::from("FAKE_GROK_WRITE_STATE"), OsString::from("1")),
        ],
    );

    client
        .search(
            "hold activity lock through cleanup",
            ResponseFormat::Concise,
        )
        .await
        .unwrap();
    watcher.join().unwrap();

    assert!(
        !lock_was_acquired.load(Ordering::SeqCst),
        "activity lock became available before the runtime disappeared"
    );
}

#[tokio::test]
async fn cleanup_failure_adds_a_path_free_warning_and_is_retried() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    let client = client_with_runtime(
        "search-success",
        TEST_TIMEOUT,
        2,
        &runtime_root,
        [(
            OsString::from("FAKE_GROK_BREAK_RUNTIME"),
            OsString::from("1"),
        )],
    );

    let output = client
        .search("deferred cleanup", ResponseFormat::Concise)
        .await
        .expect("cleanup failure must not discard a successful search");

    assert_eq!(output.warnings.len(), 1);
    assert_eq!(
        serde_json::to_value(output.warnings[0].code).unwrap(),
        "CLEANUP_DEFERRED"
    );
    let serialized = serde_json::to_string(&output).unwrap();
    assert!(serialized.contains("cleanup will be retried on the next plugin invocation"));
    assert!(!serialized.contains(runtime_root.to_string_lossy().as_ref()));
    assert_eq!(runtime_entries(&runtime_root).len(), 1);

    let retry_client = client_with_runtime("search-success", TEST_TIMEOUT, 2, &runtime_root, []);
    let retry_output = retry_client
        .search("retry deferred cleanup", ResponseFormat::Concise)
        .await
        .unwrap();
    assert!(retry_output.warnings.is_empty());
    assert!(runtime_entries(&runtime_root).is_empty());
}

#[tokio::test]
async fn exit_error_redacts_prompt_path_and_credentials() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("invocation.json");
    let client = client_with(
        "exit-failed",
        TEST_TIMEOUT,
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
        ("bad-json", ErrorCode::BadGrokJson, TEST_TIMEOUT),
        ("exit-failed", ErrorCode::GrokExitFailed, TEST_TIMEOUT),
        ("no-sources", ErrorCode::NoSources, TEST_TIMEOUT),
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
    let temp = TempDir::new().unwrap();
    let timing_log = temp.path().join("timing.jsonl");
    let client = client_with(
        "sleep",
        TEST_TIMEOUT,
        2,
        [
            (
                OsString::from("FAKE_GROK_SLEEP_SECONDS"),
                OsString::from("0.20"),
            ),
            (
                OsString::from("FAKE_GROK_TIMING_LOG"),
                timing_log.clone().into_os_string(),
            ),
        ],
    );

    let first = client.search("first", ResponseFormat::Concise);
    let second = client.search("second", ResponseFormat::Concise);
    let third = client.search("third", ResponseFormat::Concise);
    let (first, second, third) = tokio::join!(first, second, third);

    first.unwrap();
    second.unwrap();
    third.unwrap();

    let mut events: Vec<(u64, i32)> = fs::read_to_string(timing_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .map(|event| {
            let delta = match event["event"].as_str().unwrap() {
                "start" => 1,
                "end" => -1,
                value => panic!("unexpected timing event {value}"),
            };
            (event["time_ns"].as_u64().unwrap(), delta)
        })
        .collect();
    assert_eq!(events.len(), 6, "three processes must start and finish");
    events.sort_unstable();

    let mut active = 0;
    let mut peak = 0;
    for (_time, delta) in events {
        active += delta;
        assert!(active >= 0, "end event appeared before its start event");
        peak = peak.max(active);
    }
    assert_eq!(active, 0, "all fake Grok processes must finish");
    assert_eq!(peak, 2, "semaphore must permit exactly two processes");
}
