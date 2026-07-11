use std::{collections::BTreeMap, ffi::OsString, path::Path, time::Duration};

use grok_build_search_mcp::{
    DoctorInput, GrokClient, GrokConfig, GrokMcpServer, ResponseFormat, SearchService,
    WebFetchInput, WebSearchInput,
};
use rmcp::handler::server::wrapper::{Json, Parameters};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn server_with(mode: &str) -> GrokMcpServer {
    let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-grok");
    let environment = BTreeMap::from([(OsString::from("FAKE_GROK_MODE"), OsString::from(mode))]);
    let client = GrokClient::new(
        GrokConfig::new(binary)
            .with_timeout(TEST_TIMEOUT)
            .with_environment(environment),
    )
    .unwrap();
    GrokMcpServer::new(SearchService::new(client))
}

#[test]
fn router_exposes_exactly_three_structured_tools() {
    let mut tools = GrokMcpServer::tool_router().list_all();
    tools.sort_by(|left, right| left.name.cmp(&right.name));

    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, ["doctor", "web_fetch", "web_search"]);
    for tool in tools {
        let input = serde_json::to_value(&tool.input_schema).unwrap();
        let output = serde_json::to_value(&tool.output_schema).unwrap();
        assert_eq!(input["type"], "object", "{} input schema", tool.name);
        assert!(
            output["properties"]["ok"].is_object(),
            "{} output schema",
            tool.name
        );
        assert!(output["properties"]["verified"].is_object());
        assert!(output["properties"]["sources"].is_object());
        match tool.name.as_ref() {
            "web_search" => {
                assert_eq!(input["properties"]["query"]["minLength"], 1);
                assert_eq!(input["properties"]["query"]["maxLength"], 8_000);
            }
            "web_fetch" => {
                assert_eq!(input["properties"]["instructions"]["maxLength"], 8_000);
                assert_eq!(input["properties"]["max_chars"]["minimum"], 1_000);
                assert_eq!(input["properties"]["max_chars"]["maximum"], 60_000);
            }
            "doctor" => {
                assert_eq!(input["properties"]["live_search"]["default"], false);
            }
            name => panic!("unexpected tool {name}"),
        }
    }
}

#[tokio::test]
async fn tool_methods_return_structured_success_and_error_payloads() {
    let server = server_with("search-success");

    let search_result = server
        .web_search(Parameters(WebSearchInput {
            query: "official Rust website".to_string(),
            response_format: Some(ResponseFormat::Concise),
        }))
        .await;
    let Json(search) = match search_result {
        Ok(value) => value,
        Err(_) => panic!("search should succeed"),
    };
    assert!(search.ok);

    let fetch_result = server
        .web_fetch(Parameters(WebFetchInput {
            url: "http://127.0.0.1/private".to_string(),
            instructions: None,
            max_chars: None,
        }))
        .await;
    let fetch_error = match fetch_result {
        Err(value) => value,
        Ok(_) => panic!("private fetch must be a structured tool error"),
    };
    let fetch_error = fetch_error.response();
    assert!(!fetch_error.ok);
    assert_eq!(
        serde_json::to_value(fetch_error.error.as_ref().unwrap().code).unwrap(),
        "PRIVATE_URL"
    );

    let doctor_result = server
        .doctor(Parameters(DoctorInput { live_search: false }))
        .await;
    let Json(doctor) = match doctor_result {
        Ok(value) => value,
        Err(_) => panic!("doctor should succeed"),
    };
    assert!(doctor.ok);
}
