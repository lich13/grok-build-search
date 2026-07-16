use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use grok_build_search_mcp::{
    DoctorInput, ErrorCode, GrokClient, GrokConfig, ResponseFormat, SearchService, WebFetchInput,
    WebSearchInput,
};
use serde_json::Value;
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn fake_grok() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-grok")
}

fn service_with(
    mode: &str,
    extra_environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> SearchService {
    let mut environment =
        BTreeMap::from([(OsString::from("FAKE_GROK_MODE"), OsString::from(mode))]);
    environment.extend(extra_environment);
    let client = GrokClient::new(
        GrokConfig::new(fake_grok())
            .with_timeout(TEST_TIMEOUT)
            .with_environment(environment),
    )
    .unwrap();
    SearchService::new(client)
}

#[test]
fn fetch_rejects_max_chars_outside_supported_range() {
    for max_chars in [999, 60_001] {
        let error = WebFetchInput {
            url: "https://example.com/page".to_string(),
            instructions: None,
            max_chars: Some(max_chars),
        }
        .validate()
        .expect_err("out-of-range max_chars must fail");

        assert_eq!(error.code, ErrorCode::InvalidMaxChars);
    }
}

#[test]
fn fetch_rejects_instructions_over_8000_characters() {
    let error = WebFetchInput {
        url: "https://example.com/page".to_string(),
        instructions: Some("界".repeat(8_001)),
        max_chars: None,
    }
    .validate()
    .expect_err("oversized instructions must fail");

    assert_eq!(error.code, ErrorCode::InvalidInstructions);
}

#[tokio::test]
async fn fetch_rejects_private_url_before_starting_grok() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("must-not-exist.json");
    let service = service_with(
        "exit-failed",
        [(
            OsString::from("FAKE_GROK_LOG"),
            log_path.clone().into_os_string(),
        )],
    );

    let error = service
        .web_fetch(WebFetchInput {
            url: "http://127.0.0.1/admin".to_string(),
            instructions: None,
            max_chars: None,
        })
        .await
        .expect_err("private URL must fail before Grok");

    assert_eq!(error.code, ErrorCode::PrivateUrl);
    assert!(!log_path.exists());
}

#[tokio::test]
async fn fetch_uses_public_url_and_optional_instructions_via_prompt_file() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("fetch.json");
    let service = service_with(
        "search-success",
        [(
            OsString::from("FAKE_GROK_LOG"),
            log_path.clone().into_os_string(),
        )],
    );
    let url = "https://93.184.216.34/page";

    let output = service
        .web_fetch(WebFetchInput {
            url: url.to_string(),
            instructions: Some("Extract only release details".to_string()),
            max_chars: Some(1_000),
        })
        .await
        .expect("public fetch should succeed");

    assert!(output.ok);
    assert_eq!(output.sources[0].url, url);
    assert!(output.answer.chars().count() <= 1_000);
    let invocation: Value = serde_json::from_slice(&std::fs::read(log_path).unwrap()).unwrap();
    let prompt = invocation["prompt"].as_str().unwrap();
    assert!(prompt.contains(url));
    assert!(prompt.contains("Extract only release details"));
    assert!(!invocation["args"].to_string().contains(url));
}

#[tokio::test]
async fn doctor_defaults_to_version_probe_without_live_search() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("must-not-exist.json");
    let runtime_root = temp.path().join("runtimes");
    #[cfg(unix)]
    {
        std::fs::create_dir(&runtime_root).unwrap();
        std::fs::set_permissions(&runtime_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let stale = runtime_root.join("grok-build-search-runtime-abandoned");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join(".active.lock"), "").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(
        stale.join(".active.lock"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let environment = BTreeMap::from([
        (
            OsString::from("FAKE_GROK_MODE"),
            OsString::from("exit-failed"),
        ),
        (
            OsString::from("FAKE_GROK_LOG"),
            log_path.clone().into_os_string(),
        ),
    ]);
    let client = GrokClient::new(
        GrokConfig::new(fake_grok())
            .with_timeout(TEST_TIMEOUT)
            .with_runtime_root(&runtime_root)
            .with_environment(environment),
    )
    .unwrap();
    let service = SearchService::new(client);

    let output = service
        .doctor(DoctorInput { live_search: false })
        .await
        .expect("version probe should pass without live search");

    assert!(output.ok);
    assert!(output.verified);
    assert!(output.answer.contains("0.2.93"));
    assert!(
        output
            .answer
            .contains("runtime protocol compatibility was not exercised")
    );
    assert!(!output.answer.contains("installed and supported"));
    assert!(output.sources.is_empty());
    assert!(output.warnings.is_empty());
    assert!(!log_path.exists());
    assert!(!stale.exists());
    assert!(std::fs::read_dir(runtime_root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("grok-build-search-runtime-")
    }));
}

#[tokio::test]
async fn doctor_reports_deferred_cleanup_from_an_isolated_version_probe() {
    let temp = TempDir::new().unwrap();
    let runtime_root = temp.path().join("runtimes");
    let environment = BTreeMap::from([
        (
            OsString::from("FAKE_GROK_MODE"),
            OsString::from("search-success"),
        ),
        (
            OsString::from("FAKE_GROK_BREAK_RUNTIME"),
            OsString::from("1"),
        ),
    ]);
    let client = GrokClient::new(
        GrokConfig::new(fake_grok())
            .with_timeout(TEST_TIMEOUT)
            .with_runtime_root(&runtime_root)
            .with_environment(environment),
    )
    .unwrap();
    let service = SearchService::new(client);

    let output = service
        .doctor(DoctorInput { live_search: false })
        .await
        .expect("version probe should preserve its successful doctor response");

    assert!(output.ok);
    assert_eq!(output.warnings.len(), 1);
    assert_eq!(
        serde_json::to_value(output.warnings[0].code).unwrap(),
        "CLEANUP_DEFERRED"
    );
    let serialized = serde_json::to_string(&output).unwrap();
    assert!(serialized.contains("cleanup will be retried on the next plugin invocation"));
    assert!(!serialized.contains(runtime_root.to_string_lossy().as_ref()));
    for entry in std::fs::read_dir(&runtime_root).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("grok-build-search-runtime-"))
        {
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[tokio::test]
async fn doctor_live_search_returns_verified_sources() {
    let service = service_with("search-success", []);

    let output = service
        .doctor(DoctorInput { live_search: true })
        .await
        .expect("live doctor should search");

    assert!(output.ok);
    assert!(output.verified);
    assert!(!output.sources.is_empty());
}

#[tokio::test]
async fn search_validates_then_delegates_to_grok() {
    let service = service_with("search-success", []);

    let output = service
        .web_search(WebSearchInput {
            query: "current Rust language site".to_string(),
            response_format: Some(ResponseFormat::Detailed),
        })
        .await
        .expect("valid search should succeed");
    assert!(output.ok);

    let error = service
        .web_search(WebSearchInput {
            query: String::new(),
            response_format: None,
        })
        .await
        .expect_err("empty search must fail before Grok");
    assert_eq!(error.code, ErrorCode::InvalidQuery);
}
