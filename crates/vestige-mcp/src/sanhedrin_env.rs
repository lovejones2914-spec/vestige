//! The Sanhedrin verifier environment file.
//!
//! One file, `~/.claude/hooks/vestige-sanhedrin.env`, tells the Stop hook
//! which OpenAI-compatible endpoint verifies a turn. OrcaRouter is a named
//! provider on that file: its endpoint, backend label and default model are
//! written here, while its API key stays in the environment (or in a line of
//! this same 0600 file) and is never echoed, logged or returned to a caller.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// OrcaRouter's OpenAI-compatible chat completions endpoint.
pub const ORCAROUTER_CHAT_ENDPOINT: &str = "https://api.orcarouter.ai/v1/chat/completions";
/// The catalog every model choice is read from.
pub const ORCAROUTER_MODELS_URL: &str = "https://api.orcarouter.ai/v1/models";
/// The same catalog narrowed to chat models. The gateway's `capability`
/// filter and the per-entry `supported_endpoint_types` check are applied
/// together, so a model that is not a chat model never reaches the picker.
pub const ORCAROUTER_CHAT_MODELS_URL: &str = "https://api.orcarouter.ai/v1/models?capability=chat";
/// OrcaRouter's product home (key creation, account, docs).
pub const ORCAROUTER_HOME: &str = "https://www.orcarouter.ai";
/// The Sanhedrin backend label for an OpenAI-compatible gateway.
pub const ORCAROUTER_BACKEND: &str = "openai";
/// Default verifier model: an OrcaRouter-owned routing model, so a fresh
/// install verifies with one id and no provider-specific setup.
pub const ORCAROUTER_DEFAULT_MODEL: &str = "orcarouter/fusion";
/// The key environment variable the hook reads.
pub const ORCAROUTER_KEY_ENV: &str = "VESTIGE_SANHEDRIN_API_KEY";
/// Every OrcaRouter key starts with this.
pub const ORCAROUTER_KEY_PREFIX: &str = "sk-orca-";

/// Env keys this module owns when it rewrites the file. Everything else —
/// notably the API key — is preserved byte for byte.
const MANAGED_KEYS: [&str; 7] = [
    "VESTIGE_SANHEDRIN_ENABLED",
    "VESTIGE_SANHEDRIN_ENDPOINT",
    "VESTIGE_SANHEDRIN_MODEL",
    "VESTIGE_DASHBOARD_PORT",
    "VESTIGE_SANHEDRIN_CLAIM_MODE",
    "VESTIGE_SANHEDRIN_OUTPUT",
    "VESTIGE_SANHEDRIN_BACKEND",
];

/// Where the hook reads its settings from: `VESTIGE_SANHEDRIN_ENV` when set,
/// else `$HOME/.claude/hooks/vestige-sanhedrin.env`.
pub fn default_env_path() -> PathBuf {
    if let Some(path) = std::env::var_os("VESTIGE_SANHEDRIN_ENV") {
        return PathBuf::from(path);
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home)
            .join(".claude")
            .join("hooks")
            .join("vestige-sanhedrin.env"),
        None => PathBuf::from("vestige-sanhedrin.env"),
    }
}

/// A value from the file, with the quoting `quote_shell_env` writes undone.
fn file_value(contents: &str, key: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
            let value = rest.trim();
            let value = value
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .map(|v| v.replace("'\\''", "'"))
                .unwrap_or_else(|| value.to_string());
            return Some(value);
        }
    }
    None
}

/// What the hook would use right now, as the dashboard may show it. The key
/// itself is reduced to presence, never returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanhedrinProviderState {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub backend: Option<String>,
    pub key_present: bool,
    /// `environment` or `env_file` — where a key was found.
    pub key_source: Option<&'static str>,
}

/// Read the settings a verifier call would use. A missing file is empty
/// settings, not an error.
pub fn read_state(path: &Path) -> io::Result<SanhedrinProviderState> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let key_from_env = std::env::var(ORCAROUTER_KEY_ENV)
        .ok()
        .filter(|key| key.starts_with(ORCAROUTER_KEY_PREFIX));
    let key_from_file = file_value(&contents, ORCAROUTER_KEY_ENV)
        .filter(|key| key.starts_with(ORCAROUTER_KEY_PREFIX));
    let key_source = if key_from_env.is_some() {
        Some("environment")
    } else if key_from_file.is_some() {
        Some("env_file")
    } else {
        None
    };
    Ok(SanhedrinProviderState {
        endpoint: file_value(&contents, "VESTIGE_SANHEDRIN_ENDPOINT").filter(|v| !v.is_empty()),
        model: file_value(&contents, "VESTIGE_SANHEDRIN_MODEL").filter(|v| !v.is_empty()),
        backend: file_value(&contents, "VESTIGE_SANHEDRIN_BACKEND").filter(|v| !v.is_empty()),
        key_present: key_source.is_some(),
        key_source,
    })
}

/// The OrcaRouter key, from the environment first and then from the 0600 env
/// file the hook already reads. Only a key with the provider prefix is
/// accepted; anything else is treated as unset.
pub fn api_key(path: &Path) -> Option<String> {
    if let Ok(key) = std::env::var(ORCAROUTER_KEY_ENV)
        && key.starts_with(ORCAROUTER_KEY_PREFIX)
    {
        return Some(key);
    }
    let contents = fs::read_to_string(path).ok()?;
    file_value(&contents, ORCAROUTER_KEY_ENV).filter(|key| key.starts_with(ORCAROUTER_KEY_PREFIX))
}

/// Store the OrcaRouter key in the 0600 env file the hook reads. The key is
/// written once and never returned: no caller can read it back through this
/// module, and an error never carries it. A value without the provider
/// prefix is refused so a mistyped paste cannot become the verifier's key.
pub fn write_key(path: &Path, key: &str) -> io::Result<()> {
    let key = key.trim();
    if !key.starts_with(ORCAROUTER_KEY_PREFIX)
        || key.len() < ORCAROUTER_KEY_PREFIX.len() + 8
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("an OrcaRouter key starts with {ORCAROUTER_KEY_PREFIX}; nothing was written"),
        ));
    }
    let existing = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let mut contents = String::new();
    for line in existing.lines() {
        let name = line.split_once('=').map(|(name, _)| name).unwrap_or(line);
        if name == ORCAROUTER_KEY_ENV {
            continue;
        }
        contents.push_str(line);
        contents.push('\n');
    }
    contents.push_str(&format!("{ORCAROUTER_KEY_ENV}={}\n", quote_shell_env(key)));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn quote_shell_env(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Point the verifier at OrcaRouter (or at another named OpenAI-compatible
/// provider) without touching the key. An empty `backend` or `dashboard_port`
/// keeps what is already on disk, so a change made from the dashboard cannot
/// silently drop a value the installer wrote.
pub fn write_provider(
    path: &Path,
    endpoint: &str,
    model: &str,
    backend: &str,
    dashboard_port: &str,
) -> io::Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let kept_port = file_value(&existing, "VESTIGE_DASHBOARD_PORT")
        .filter(|port| !port.is_empty())
        .unwrap_or_else(|| dashboard_port.to_string());
    let mut contents = String::new();
    for line in existing.lines() {
        let key = line.split_once('=').map(|(key, _)| key).unwrap_or(line);
        let managed = MANAGED_KEYS.contains(&key)
            && !(key == "VESTIGE_SANHEDRIN_BACKEND" && backend.is_empty())
            && !(key == "VESTIGE_DASHBOARD_PORT" && dashboard_port.is_empty());
        if !managed {
            contents.push_str(line);
            contents.push('\n');
        }
    }
    contents.push_str("VESTIGE_SANHEDRIN_ENABLED=1\n");
    contents.push_str(&format!(
        "VESTIGE_SANHEDRIN_ENDPOINT={}\n",
        quote_shell_env(endpoint)
    ));
    contents.push_str(&format!(
        "VESTIGE_SANHEDRIN_MODEL={}\n",
        quote_shell_env(model)
    ));
    contents.push_str(&format!(
        "VESTIGE_DASHBOARD_PORT={}\n",
        quote_shell_env(&kept_port)
    ));
    contents.push_str("VESTIGE_SANHEDRIN_CLAIM_MODE=1\n");
    contents.push_str("VESTIGE_SANHEDRIN_OUTPUT=json\n");
    if !backend.is_empty() {
        contents.push_str(&format!(
            "VESTIGE_SANHEDRIN_BACKEND={}\n",
            quote_shell_env(backend)
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_env(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vestige-sanhedrin-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("vestige-sanhedrin.env")
    }

    #[test]
    fn writing_orcarouter_keeps_the_key_line() {
        let path = temp_env("keeps-key");
        fs::write(
            &path,
            "VESTIGE_SANHEDRIN_API_KEY='sk-orca-not-a-real-key'\nVESTIGE_SANHEDRIN_MODEL='old'\n",
        )
        .unwrap();

        write_provider(
            &path,
            ORCAROUTER_CHAT_ENDPOINT,
            "orcarouter/fusion",
            ORCAROUTER_BACKEND,
            "3927",
        )
        .unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("VESTIGE_SANHEDRIN_API_KEY='sk-orca-not-a-real-key'"));
        assert!(written.contains("VESTIGE_SANHEDRIN_MODEL='orcarouter/fusion'"));
        assert!(written.contains("VESTIGE_SANHEDRIN_BACKEND='openai'"));
        assert!(!written.contains("VESTIGE_SANHEDRIN_MODEL='old'"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_empty_backend_keeps_the_existing_label() {
        let path = temp_env("keeps-backend");
        fs::write(&path, "VESTIGE_SANHEDRIN_BACKEND='ollama'\n").unwrap();
        write_provider(
            &path,
            "http://127.0.0.1:11434/v1/chat/completions",
            "m",
            "",
            "3927",
        )
        .unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("VESTIGE_SANHEDRIN_BACKEND='ollama'")
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn state_reports_presence_not_the_key() {
        let path = temp_env("state");
        fs::write(
            &path,
            "VESTIGE_SANHEDRIN_ENDPOINT='https://api.orcarouter.ai/v1/chat/completions'\nVESTIGE_SANHEDRIN_MODEL='orcarouter/fusion'\nVESTIGE_SANHEDRIN_BACKEND='openai'\nVESTIGE_SANHEDRIN_API_KEY='sk-orca-not-a-real-key'\n",
        )
        .unwrap();
        let state = read_state(&path).unwrap();
        assert_eq!(state.model.as_deref(), Some("orcarouter/fusion"));
        assert_eq!(state.backend.as_deref(), Some("openai"));
        assert_eq!(
            state.endpoint.as_deref(),
            Some("https://api.orcarouter.ai/v1/chat/completions")
        );
        assert!(state.key_present);
        let debug = format!("{state:?}");
        assert!(!debug.contains("not-a-real-key"), "{debug}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn writing_a_key_keeps_the_rest_of_the_file_and_never_echoes_it() {
        let path = temp_env("writes-key");
        fs::write(&path, "VESTIGE_SANHEDRIN_MODEL='orcarouter/fusion'\n").unwrap();
        write_key(&path, "  sk-orca-test-key-0123456789  ").unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("VESTIGE_SANHEDRIN_MODEL='orcarouter/fusion'"));
        assert!(written.contains("VESTIGE_SANHEDRIN_API_KEY='sk-orca-test-key-0123456789'"));
        write_key(&path, "sk-orca-second-key-0123456789").unwrap();
        let rewritten = fs::read_to_string(&path).unwrap();
        assert_eq!(rewritten.matches("VESTIGE_SANHEDRIN_API_KEY=").count(), 1);
        assert!(!rewritten.contains("test-key"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_key_without_the_prefix_is_refused_and_not_written() {
        let path = temp_env("refuses-key");
        fs::write(&path, "VESTIGE_SANHEDRIN_MODEL='m'\n").unwrap();
        let err = write_key(&path, "sk-live-abc").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!err.to_string().contains("sk-live-abc"));
        assert!(!fs::read_to_string(&path).unwrap().contains("API_KEY"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_short_key_does_not_count_as_configured() {
        let path = temp_env("short-key");
        fs::write(&path, "VESTIGE_SANHEDRIN_API_KEY='nope'\n").unwrap();
        assert!(!read_state(&path).unwrap().key_present);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
