//! The OrcaRouter surface of the dashboard.
//!
//! OrcaRouter is a named provider for the Sanhedrin verifier: an
//! OpenAI-compatible gateway whose catalog decides which models exist. This
//! module is the dashboard's half of that contract — it reads the same env
//! file the hook reads, serves the catalog the provider is configured with,
//! and writes a model choice back without ever touching the key.
//!
//! The browser never talks to `api.orcarouter.ai`: the dashboard's CSP keeps
//! `connect-src` on the local origin, so the catalog is fetched here, by the
//! backend, with the key on this machine only.

use std::io::Write;
use std::process::{Command, Stdio};

use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::Value;

use super::handlers::ApiError;
use crate::sanhedrin_env::{
    ORCAROUTER_BACKEND, ORCAROUTER_CHAT_ENDPOINT, ORCAROUTER_CHAT_MODELS_URL, ORCAROUTER_HOME,
    ORCAROUTER_KEY_PREFIX, api_key, default_env_path, read_state, write_key, write_provider,
};

/// How long the catalog fetch may take before the page is told the network
/// did not answer, so a hung proxy cannot hold a dashboard request open.
const CATALOG_TIMEOUT_SECS: u32 = 20;

/// Endpoint types that mean "a chat model this verifier can call". The
/// verifier speaks the OpenAI chat-completions shape; a model that only
/// serves images, video, embeddings or reranking cannot stand in for it.
const CHAT_ENDPOINT_TYPES: [&str; 4] = ["openai", "anthropic", "gemini", "openai-response"];
/// Endpoint types that disqualify a model even when it also lists a chat one.
const NON_CHAT_ENDPOINT_TYPES: [&str; 4] = [
    "image-generation",
    "openai-video",
    "jina-rerank",
    "embeddings",
];
/// Parameters that make a model usable as a chat verifier. A model with a
/// parameter list that offers none of these is not a chat model, whatever
/// endpoint it advertises.
const CHAT_PARAMETERS: [&str; 7] = [
    "max_tokens",
    "max_completion_tokens",
    "messages",
    "temperature",
    "tools",
    "tool_choice",
    "stream",
];

/// What the dashboard may show about the provider. The key is reduced to
/// presence and its source; it is never part of this response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStateView {
    pub configured: bool,
    pub endpoint: String,
    pub model: Option<String>,
    pub backend: String,
    pub key_present: bool,
    pub key_source: Option<&'static str>,
    pub home: &'static str,
    pub env_path: String,
}

/// Read the provider state the hook would use right now.
pub fn provider_state(env_path: &std::path::Path) -> std::io::Result<ProviderStateView> {
    let state = read_state(env_path)?;
    Ok(ProviderStateView {
        configured: state.endpoint.is_some(),
        endpoint: state
            .endpoint
            .unwrap_or_else(|| ORCAROUTER_CHAT_ENDPOINT.to_string()),
        model: state.model,
        backend: state
            .backend
            .unwrap_or_else(|| ORCAROUTER_BACKEND.to_string()),
        key_present: state.key_present,
        key_source: state.key_source,
        home: ORCAROUTER_HOME,
        env_path: env_path.display().to_string(),
    })
}

/// Why a catalog fetch failed, in the words the page shows. Each variant
/// maps to a stable code so the UI can offer the right next step (paste a
/// key, fix the key, refresh) instead of a generic failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFailure {
    /// No usable key on this machine.
    KeyMissing,
    /// The gateway rejected the key (401/403).
    KeyRejected,
    /// The gateway answered with a server error.
    Upstream,
    /// Everything else: DNS, TLS, timeout, no `curl`, truncated body.
    Unreachable,
    /// A model this key's catalog does not serve.
    UnknownModel,
}

impl CatalogFailure {
    fn code(self) -> &'static str {
        match self {
            Self::KeyMissing => "orcarouter_key_missing",
            Self::KeyRejected => "orcarouter_key_rejected",
            Self::Upstream => "orcarouter_upstream_error",
            Self::Unreachable => "orcarouter_unreachable",
            Self::UnknownModel => "orcarouter_model_unavailable",
        }
    }

    fn message(self) -> String {
        match self {
            Self::KeyMissing => format!(
                "No OrcaRouter key is configured yet. Export VESTIGE_SANHEDRIN_API_KEY (keys start with {ORCAROUTER_KEY_PREFIX}) and refresh."
            ),
            Self::KeyRejected => {
                "OrcaRouter rejected the configured key. Check it at www.orcarouter.ai and refresh."
                    .to_string()
            }
            Self::Upstream => {
                "OrcaRouter answered with a server error. The catalog is unchanged; refresh to try again.".to_string()
            }
            Self::Unreachable => {
                "Could not reach api.orcarouter.ai. Check this machine's network and refresh.".to_string()
            }
            Self::UnknownModel => {
                "That model is not in the catalog this key can reach; pick one from the list."
                    .to_string()
            }
        }
    }

    fn status(self) -> StatusCode {
        match self {
            Self::KeyMissing => StatusCode::BAD_REQUEST,
            Self::KeyRejected => StatusCode::UNAUTHORIZED,
            Self::Upstream => StatusCode::BAD_GATEWAY,
            Self::Unreachable => StatusCode::SERVICE_UNAVAILABLE,
            Self::UnknownModel => StatusCode::BAD_REQUEST,
        }
    }

    fn into_error(self) -> ApiError {
        ApiError::reason(self.status(), self.code(), self.message())
    }
}

/// The catalog as the dashboard shows it: only models this verifier can
/// actually call, with the fields the dropdown needs and nothing that would
/// let the page invent a model the gateway does not serve.
pub fn chat_models(catalog: &Value) -> Vec<Value> {
    let Some(entries) = catalog.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter(|entry| is_chat_model(entry))
        .map(|entry| {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let label = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| id.clone());
            serde_json::json!({
                "id": id,
                "label": label,
                "ownedBy": entry.get("owned_by").cloned().unwrap_or(Value::Null),
                "contextLength": entry.get("context_length").cloned().unwrap_or(Value::Null),
                "supportsReasoning": supports_reasoning(entry),
            })
        })
        .collect()
}

fn string_list(entry: &Value, key: &str) -> Option<Vec<String>> {
    entry.get(key).and_then(Value::as_array).map(|values| {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

/// A model the verifier can call: it must advertise one of the chat endpoint
/// types, must not advertise an exclusive one, and — when the catalog lists
/// its parameters — must offer at least one parameter a chat request uses.
pub fn is_chat_model(entry: &Value) -> bool {
    let Some(endpoints) = string_list(entry, "supported_endpoint_types") else {
        return false;
    };
    let chats = endpoints
        .iter()
        .any(|endpoint| CHAT_ENDPOINT_TYPES.contains(&endpoint.as_str()));
    let exclusive = endpoints
        .iter()
        .any(|endpoint| NON_CHAT_ENDPOINT_TYPES.contains(&endpoint.as_str()));
    if !chats || exclusive {
        return false;
    }
    if let Some(outputs) = entry
        .get("architecture")
        .and_then(|architecture| architecture.get("output_modalities"))
        .and_then(Value::as_array)
    {
        let text_only = outputs
            .iter()
            .filter_map(Value::as_str)
            .all(|modality| modality == "text");
        if !text_only {
            return false;
        }
    }
    match string_list(entry, "supported_parameters") {
        Some(parameters) if parameters.is_empty() => true,
        Some(parameters) => parameters
            .iter()
            .any(|parameter| CHAT_PARAMETERS.contains(&parameter.as_str())),
        None => true,
    }
}

/// Whether the catalog says this model takes a reasoning control. Derived
/// from the catalog alone — no model id is special-cased, so a model the
/// gateway stops advertising loses the control with it.
fn supports_reasoning(entry: &Value) -> bool {
    string_list(entry, "supported_parameters")
        .map(|parameters| {
            parameters
                .iter()
                .any(|parameter| parameter == "reasoning" || parameter == "reasoning_effort")
        })
        .unwrap_or(false)
}

/// Fetch the catalog with `curl` — the outbound HTTP the rest of this crate
/// already uses. The key is passed on stdin as a curl config so it never
/// appears in the process list, in a log line, or in an error.
fn fetch_catalog(key: &str) -> Result<Value, CatalogFailure> {
    let mut child = Command::new("curl")
        .arg("-K")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CatalogFailure::Unreachable)?;
    let config = format!(
        "url = \"{ORCAROUTER_CHAT_MODELS_URL}\"\n\
         header = \"Authorization: Bearer {key}\"\n\
         header = \"Accept: application/json\"\n\
         silent\n\
         show-error\n\
         max-time = {CATALOG_TIMEOUT_SECS}\n\
         write-out = \"\\n%{{http_code}}\"\n",
    );
    {
        let mut stdin = child.stdin.take().ok_or(CatalogFailure::Unreachable)?;
        if stdin.write_all(config.as_bytes()).is_err() {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(CatalogFailure::Unreachable);
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|_| CatalogFailure::Unreachable)?;
    if !output.status.success() {
        // `--fail` is not used on purpose: a 401 body names the reason, and
        // the status line is parsed below instead.
        return Err(CatalogFailure::Unreachable);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, status) = text.rsplit_once('\n').ok_or(CatalogFailure::Unreachable)?;
    match status.trim() {
        "200" => serde_json::from_str::<Value>(body).map_err(|_| CatalogFailure::Unreachable),
        "401" | "403" => Err(CatalogFailure::KeyRejected),
        code if code.starts_with('5') => Err(CatalogFailure::Upstream),
        _ => Err(CatalogFailure::Unreachable),
    }
}

/// `GET /api/orcarouter/state` — what the verifier is configured with. The
/// key is reported as present or absent, never by value.
pub async fn get_orcarouter_state() -> Result<Json<Value>, ApiError> {
    state_for(default_env_path()).await
}

/// The body of the state route, against an explicit env file so a test reads
/// a fixture instead of the machine's own hook configuration.
async fn state_for(env_path: std::path::PathBuf) -> Result<Json<Value>, ApiError> {
    let view = tokio::task::spawn_blocking(move || provider_state(&env_path))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| ApiError::internal(&"could not read the Sanhedrin env file"))?;
    Ok(Json(serde_json::json!({
        "configured": view.configured,
        "endpoint": view.endpoint,
        "model": view.model,
        "backend": view.backend,
        "keyPresent": view.key_present,
        "keySource": view.key_source,
        "home": view.home,
        "envPath": view.env_path,
        "modelsUrl": ORCAROUTER_CHAT_MODELS_URL,
    })))
}

#[derive(Deserialize)]
pub struct StoreKeyRequest {
    /// The key as pasted. It is written to the 0600 env file and never
    /// returned, logged or included in an error.
    pub key: String,
}

/// `POST /api/orcarouter/key` — store a pasted key in the hook's env file.
/// The response says only that a key is now present and where it came from.
pub async fn store_orcarouter_key(
    Json(request): Json<StoreKeyRequest>,
) -> Result<Json<Value>, ApiError> {
    store_key_for(default_env_path(), request).await
}

async fn store_key_for(
    env_path: std::path::PathBuf,
    request: StoreKeyRequest,
) -> Result<Json<Value>, ApiError> {
    let key = request.key;
    let written = tokio::task::spawn_blocking(move || {
        write_key(&env_path, &key)?;
        read_state(&env_path).map_err(|err| std::io::Error::other(err.to_string()))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match written {
        Ok(state) => Ok(Json(serde_json::json!({
            "keyPresent": state.key_present,
            "keySource": state.key_source,
        }))),
        Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => Err(ApiError::reason(
            StatusCode::BAD_REQUEST,
            "orcarouter_key_invalid",
            "That does not look like an OrcaRouter key: keys start with sk-orca-. Nothing was written."
                .to_string(),
        )),
        Err(_) => Err(ApiError::reason(
            StatusCode::INTERNAL_SERVER_ERROR,
            "orcarouter_key_not_saved",
            "Could not write the Sanhedrin env file. Check permissions on ~/.claude/hooks."
                .to_string(),
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct ChooseModelRequest {
    /// The model id exactly as the catalog served it.
    pub model: String,
}

/// `GET /api/orcarouter/models` — the live catalog, filtered to what the
/// verifier can call. A failure answers 400/401/502/503 with a code the page
/// turns into its own message, so it is never mistaken for an empty catalog.
pub async fn get_orcarouter_models() -> Result<Json<Value>, ApiError> {
    models_for(default_env_path()).await
}

/// The body of the catalog route, against an explicit env file.
async fn models_for(env_path: std::path::PathBuf) -> Result<Json<Value>, ApiError> {
    let key = tokio::task::spawn_blocking(move || api_key(&env_path))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    catalog_for(key).await
}

/// The catalog body for a key that was already resolved, so the no-key path
/// is testable without touching the network or the machine's environment.
async fn catalog_for(key: Option<String>) -> Result<Json<Value>, ApiError> {
    let fetched = tokio::task::spawn_blocking(move || {
        let key = key.ok_or(CatalogFailure::KeyMissing)?;
        fetch_catalog(&key)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let catalog = fetched.map_err(CatalogFailure::into_error)?;
    let models = chat_models(&catalog);
    Ok(Json(serde_json::json!({
        "models": models,
        "total": models.len(),
        "catalogTotal": catalog.get("data").and_then(Value::as_array).map(Vec::len),
        "source": ORCAROUTER_CHAT_MODELS_URL,
        "fetchedAt": chrono::Utc::now().to_rfc3339(),
    })))
}

/// `POST /api/orcarouter/model` — persist the model the verifier should call.
/// Only a model the catalog currently serves is accepted: the dropdown is
/// the only source of truth, and a stale or invented id is refused rather
/// than written into a file the hook would then fail on.
pub async fn choose_orcarouter_model(
    Json(request): Json<ChooseModelRequest>,
) -> Result<Json<Value>, ApiError> {
    choose_for(default_env_path(), request).await
}

/// The body of the model-choice route, against an explicit env file.
async fn choose_for(
    env_path: std::path::PathBuf,
    request: ChooseModelRequest,
) -> Result<Json<Value>, ApiError> {
    let model = request.model.trim().to_string();
    if model.is_empty() {
        return Err(ApiError::reason(
            StatusCode::BAD_REQUEST,
            "orcarouter_model_empty",
            "Choose a model from the OrcaRouter catalog first.".to_string(),
        ));
    }
    let chosen = model.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let key = api_key(&env_path).ok_or(CatalogFailure::KeyMissing)?;
        let catalog = fetch_catalog(&key)?;
        if !chat_models(&catalog)
            .iter()
            .any(|entry| entry["id"].as_str() == Some(chosen.as_str()))
        {
            return Err(CatalogFailure::UnknownModel);
        }
        let state = read_state(&env_path).map_err(|_| CatalogFailure::Unreachable)?;
        let endpoint = state
            .endpoint
            .unwrap_or_else(|| ORCAROUTER_CHAT_ENDPOINT.to_string());
        let backend = state
            .backend
            .unwrap_or_else(|| ORCAROUTER_BACKEND.to_string());
        let port = std::env::var("VESTIGE_DASHBOARD_PORT").unwrap_or_default();
        write_provider(&env_path, &endpoint, &chosen, &backend, &port)
            .map_err(|_| CatalogFailure::Unreachable)?;
        Ok::<String, CatalogFailure>(chosen)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    outcome
        .map(|model| Json(serde_json::json!({ "model": model, "saved": true })))
        .map_err(CatalogFailure::into_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog(entries: Vec<Value>) -> Value {
        json!({ "object": "list", "success": true, "data": entries })
    }

    fn entry(id: &str, endpoints: &[&str]) -> Value {
        json!({ "id": id, "object": "model", "supported_endpoint_types": endpoints })
    }

    #[test]
    fn only_chat_endpoint_types_survive() {
        let models = chat_models(&catalog(vec![
            entry("openai/gpt-5.5", &["openai", "openai-response"]),
            entry("anthropic/claude-opus-4.8", &["openai", "anthropic"]),
            entry("google/gemini-3.5-flash", &["gemini"]),
            entry("some/embedding", &["embeddings"]),
            entry("some/rerank", &["jina-rerank"]),
            entry("some/video", &["openai-video"]),
            entry("some/image", &["openai", "image-generation"]),
            entry("some/unknown", &[]),
        ]));
        let ids: Vec<&str> = models
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "openai/gpt-5.5",
                "anthropic/claude-opus-4.8",
                "google/gemini-3.5-flash"
            ]
        );
    }

    #[test]
    fn an_image_only_output_is_not_a_chat_model() {
        let image = json!({
            "id": "google/gemini-3-pro-image-preview",
            "supported_endpoint_types": ["openai", "google"],
            "architecture": { "output_modalities": ["image", "text"] },
        });
        let text = json!({
            "id": "openai/gpt-5.5",
            "supported_endpoint_types": ["openai"],
            "architecture": { "input_modalities": ["text", "image"], "output_modalities": ["text"] },
        });
        let models = chat_models(&catalog(vec![image, text]));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "openai/gpt-5.5");
    }

    #[test]
    fn a_parameter_list_without_a_chat_parameter_is_not_a_chat_model() {
        let not_chat = json!({
            "id": "weird/model",
            "supported_endpoint_types": ["openai"],
            "supported_parameters": ["image_size", "n"],
        });
        let chat = json!({
            "id": "openai/gpt-5.5",
            "supported_endpoint_types": ["openai"],
            "supported_parameters": ["max_tokens", "tools"],
        });
        let models = chat_models(&catalog(vec![not_chat, chat]));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "openai/gpt-5.5");
    }

    #[test]
    fn the_label_is_the_catalog_name_or_the_id() {
        let named = json!({
            "id": "openai/gpt-5.5", "name": "OpenAI: GPT-5.5",
            "supported_endpoint_types": ["openai"],
        });
        let bare = json!({ "id": "orcarouter/fusion", "supported_endpoint_types": ["openai"] });
        let models = chat_models(&catalog(vec![named, bare]));
        assert_eq!(models[0]["label"], "OpenAI: GPT-5.5");
        assert_eq!(models[1]["label"], "orcarouter/fusion");
    }

    #[test]
    fn reasoning_comes_from_the_catalog_not_from_a_model_name() {
        let reasoning = json!({
            "id": "openai/gpt-5.5",
            "supported_endpoint_types": ["openai"],
            "supported_parameters": ["reasoning", "max_tokens"],
        });
        let plain = json!({
            "id": "orcarouter/fusion",
            "supported_endpoint_types": ["openai"],
        });
        let models = chat_models(&catalog(vec![reasoning, plain]));
        assert_eq!(models[0]["supportsReasoning"], true);
        assert_eq!(models[1]["supportsReasoning"], false);
    }

    #[test]
    fn a_malformed_catalog_is_an_empty_list_not_a_panic() {
        assert!(chat_models(&json!({ "error": "nope" })).is_empty());
        assert!(chat_models(&json!({ "data": "nope" })).is_empty());
    }

    #[test]
    fn state_reports_the_provider_without_the_key() {
        let dir =
            std::env::temp_dir().join(format!("vestige-orcarouter-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vestige-sanhedrin.env");
        std::fs::write(
            &path,
            "VESTIGE_SANHEDRIN_ENDPOINT='https://api.orcarouter.ai/v1/chat/completions'\nVESTIGE_SANHEDRIN_MODEL='orcarouter/fusion'\nVESTIGE_SANHEDRIN_BACKEND='openai'\nVESTIGE_SANHEDRIN_API_KEY='sk-orca-not-a-real-key'\n",
        )
        .unwrap();
        let view = provider_state(&path).unwrap();
        assert!(view.configured);
        assert!(view.key_present);
        assert_eq!(view.key_source, Some("env_file"));
        assert_eq!(view.model.as_deref(), Some("orcarouter/fusion"));
        assert_eq!(view.home, "https://www.orcarouter.ai");
        assert!(!format!("{view:?}").contains("not-a-real-key"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_empty_model_choice_is_refused_before_any_network_call() {
        let dir = std::env::temp_dir().join(format!(
            "vestige-orcarouter-empty-choice-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("missing.env");
        let error = choose_for(
            path,
            ChooseModelRequest {
                model: "   ".to_string(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_state_route_reads_a_fixture_env_file() {
        let dir = std::env::temp_dir().join(format!(
            "vestige-orcarouter-state-route-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vestige-sanhedrin.env");
        std::fs::write(
            &path,
            "VESTIGE_SANHEDRIN_ENDPOINT='https://api.orcarouter.ai/v1/chat/completions'
VESTIGE_SANHEDRIN_MODEL='openai/gpt-5.5'
VESTIGE_SANHEDRIN_BACKEND='openai'
VESTIGE_SANHEDRIN_API_KEY='sk-orca-not-a-real-key'
",
        )
        .unwrap();
        let Json(body) = state_for(path).await.unwrap();
        assert_eq!(body["configured"], true);
        assert_eq!(body["model"], "openai/gpt-5.5");
        assert_eq!(body["keyPresent"], true);
        assert_eq!(body["keySource"], "env_file");
        assert!(!body.to_string().contains("not-a-real-key"), "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_pasted_key_is_stored_without_being_echoed_back() {
        let dir = std::env::temp_dir().join(format!(
            "vestige-orcarouter-store-key-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vestige-sanhedrin.env");
        std::fs::write(&path, "VESTIGE_SANHEDRIN_MODEL='orcarouter/fusion'\n").unwrap();
        let Json(body) = store_key_for(
            path.clone(),
            StoreKeyRequest {
                key: "sk-orca-not-a-real-key".to_string(),
            },
        )
        .await
        .unwrap();
        assert_eq!(body["keyPresent"], true);
        assert_eq!(body["keySource"], "env_file");
        assert!(!body.to_string().contains("not-a-real-key"), "{body}");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("VESTIGE_SANHEDRIN_API_KEY='sk-orca-not-a-real-key'"));
        assert!(written.contains("VESTIGE_SANHEDRIN_MODEL='orcarouter/fusion'"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_mistyped_key_is_refused_and_the_file_is_left_alone() {
        let dir =
            std::env::temp_dir().join(format!("vestige-orcarouter-bad-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vestige-sanhedrin.env");
        let before = "VESTIGE_SANHEDRIN_MODEL='orcarouter/fusion'\n";
        std::fs::write(&path, before).unwrap();
        let error = store_key_for(
            path.clone(),
            StoreKeyRequest {
                key: "sk-live-mistyped".to_string(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error, StatusCode::BAD_REQUEST);
        assert_eq!(error.code(), Some("orcarouter_key_invalid"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_catalog_request_without_a_key_never_reaches_the_network() {
        let error = catalog_for(None).await.unwrap_err();
        assert_eq!(error, StatusCode::BAD_REQUEST);
        assert_eq!(error.code(), Some("orcarouter_key_missing"));
    }

    #[test]
    fn an_unconfigured_machine_reports_the_orcarouter_defaults() {
        let dir =
            std::env::temp_dir().join(format!("vestige-orcarouter-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("missing.env");
        let view = provider_state(&path).unwrap();
        assert!(!view.configured);
        assert_eq!(view.endpoint, ORCAROUTER_CHAT_ENDPOINT);
        assert_eq!(view.backend, "openai");
        assert!(view.model.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
