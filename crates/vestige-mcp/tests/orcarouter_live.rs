//! The OrcaRouter provider against the live gateway.
//!
//! This is the one check that needs a real key. It runs only when
//! `ORCAROUTER_API_KEY` is exported; without it the test returns immediately
//! so the rest of the suite needs no network and no credential.
//!
//! The test drives the dashboard routes this change added — the same code the
//! page calls — and then proves the config they wrote actually completes:
//! `GET /api/orcarouter/models` reads the live key-scoped catalog, `POST
//! /api/orcarouter/model` persists a model the catalog served, and a
//! chat-completions call against the endpoint in the env file answers. The key
//! travels on stdin only and is never printed.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;
use vestige_core::Storage;

const PORT: u16 = 47513;
const HOST: &str = "127.0.0.1:47513";
const OWN_ORIGIN: &str = "http://127.0.0.1:47513";
const MODELS_URL: &str = "https://api.orcarouter.ai/v1/models?capability=chat";

fn key_from_environment() -> Option<String> {
    let name = ["ORCAROUTER_API_", "KEY"].concat();
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| value.starts_with("sk-orca-"))
}

fn strata(dir: &Path) -> Arc<Storage> {
    vestige_mcp::strata_memory::open(dir).unwrap()
}

struct Reply {
    status: StatusCode,
    body: String,
}

async fn own(app: &Router, method: &str, uri: &str, body: Option<&str>) -> Reply {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST)
        .header("origin", OWN_ORIGIN);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request
        .body(body.map_or_else(Body::empty, |text| Body::from(text.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// One completion against the endpoint and model the provider wrote, with the
/// key on stdin so it never appears in the process list.
fn complete(endpoint: &str, model: &str, key: &str) -> Result<Value, String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": "Reply with exactly one word: ready"
        }],
        "max_tokens": 16,
        "temperature": 0.0,
        "stream": false,
    })
    .to_string();
    let config = format!(
        "url = \"{endpoint}\"\n\
         header = \"Authorization: Bearer {key}\"\n\
         header = \"Content-Type: application/json\"\n\
         data = \"{}\"\n\
         silent\n\
         show-error\n\
         max-time = 60\n\
         write-out = \"\\n%{{http_code}}\"\n",
        body.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let mut child = Command::new("curl")
        .arg("-K")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("curl did not start: {err}"))?;
    {
        let mut stdin = child.stdin.take().ok_or("curl has no stdin")?;
        stdin
            .write_all(config.as_bytes())
            .map_err(|err| format!("curl config was not accepted: {err}"))?;
    }
    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err("curl failed".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, status) = text.rsplit_once('\n').ok_or("no status line")?;
    if status.trim() != "200" {
        return Err(format!("completion answered {}", status.trim()));
    }
    serde_json::from_str(body).map_err(|_| "completion body was not JSON".to_string())
}

/// Try the catalog in the order the provider served it and return the first
/// model that completes. A key's catalog can list a model its scope cannot
/// call (`403 model_access_denied`); the test reports that instead of
/// pretending the first row is callable.
fn complete_with_a_served_model(models: &[Value], key: &str) -> (String, Value, Vec<String>) {
    let mut refused = Vec::new();
    for entry in models {
        let Some(id) = entry["id"].as_str() else {
            continue;
        };
        match complete("https://api.orcarouter.ai/v1/chat/completions", id, key) {
            Ok(reply) => return (id.to_string(), reply, refused),
            Err(reason) => refused.push(format!("{id}: {reason}")),
        }
    }
    panic!("no catalog model completed: {refused:?}");
}

#[tokio::test]
async fn the_provider_reads_the_live_catalog_and_completes_with_the_chosen_model() {
    let Some(key) = key_from_environment() else {
        eprintln!("skipped: ORCAROUTER_API_KEY is not set");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("vestige-sanhedrin.env");
    // The routes read the same file the hook reads; point them at this one.
    // SAFETY: this test binary has a single test, so no other thread reads
    // the environment while it is changed.
    unsafe {
        std::env::set_var("VESTIGE_SANHEDRIN_ENV", &env_path);
        std::env::remove_var("VESTIGE_SANHEDRIN_API_KEY");
    }

    let storage = strata(dir.path());
    let app = vestige_mcp::dashboard::build_router(Arc::clone(&storage), None, PORT).0;

    // 0. The key goes in through the route the page posts to. It is written
    //    to the 0600 file and never comes back.
    let stored = own(
        &app,
        "POST",
        "/api/orcarouter/key",
        Some(&serde_json::json!({ "key": key }).to_string()),
    )
    .await;
    assert_eq!(stored.status, StatusCode::OK, "{}", stored.body);
    assert!(
        !stored.body.contains("sk-orca-"),
        "the route echoed the key"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&env_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // 1. The catalog comes from the provider's own /v1/models, with the key
    //    this machine holds — not from a list checked into the repository.
    let catalog = own(&app, "GET", "/api/orcarouter/models", None).await;
    assert_eq!(catalog.status, StatusCode::OK, "{}", catalog.body);
    let body: Value = serde_json::from_str(&catalog.body).unwrap();
    assert_eq!(body["source"], MODELS_URL);
    let models = body["models"].as_array().expect("models array");
    assert!(!models.is_empty(), "the live catalog is empty: {body}");
    assert_eq!(body["total"], models.len());
    assert!(
        !catalog.body.contains("sk-orca-"),
        "the catalog echoed a key"
    );

    // 2. The model choice is written into the env file the hook reads, and
    //    only a model the catalog served is accepted. The catalog can list a
    //    model this key's scope cannot call, so the completion picks the first
    //    entry that answers and is persisted through the same route.
    let (chosen, reply, refused) = complete_with_a_served_model(models, &key);
    let saved = own(
        &app,
        "POST",
        "/api/orcarouter/model",
        Some(&serde_json::json!({ "model": chosen }).to_string()),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    let written = std::fs::read_to_string(&env_path).unwrap();
    assert!(
        written.contains(&format!("VESTIGE_SANHEDRIN_MODEL='{chosen}'")),
        "model not written: {written}"
    );
    assert!(
        written
            .contains("VESTIGE_SANHEDRIN_ENDPOINT='https://api.orcarouter.ai/v1/chat/completions'"),
        "{written}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&env_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // 3. The provider the routes configured answers a real completion.
    let content = reply["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    assert!(!content.trim().is_empty(), "{reply}");
    eprintln!("live catalog models: {}", models.len());
    eprintln!("live completion from {chosen}: {} chars", content.len());
    for entry in &refused {
        eprintln!("scope denied: {entry}");
    }
}
