//! Mac-local provider configuration. No environment credential fallback.
//!
//! Two providers are supported: Google Gemini (the default, for backwards
//! compatibility) and OpenRouter. Each has its own Keychain item and model,
//! so switching provider never sends one provider's key to the other.
use std::io::{BufRead, Read, Write};
use std::path::Path;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::session::paths::LatchHome;

/// Default Gemini model, independently configurable without a mobile release.
pub const DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";
/// The same default model, addressed through OpenRouter.
pub const DEFAULT_OPENROUTER_MODEL: &str = "google/gemini-3.5-flash-lite";
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "api-key";
/// Legacy location of the Gemini model; kept so existing setups keep working.
const GEMINI_MODEL_FILE: &str = "commit-message-model";
/// Non-secret provider choice and OpenRouter model/routing preferences.
const SETTINGS_FILE: &str = "commit-message-settings.json";
const MAX_ROUTED_PROVIDERS: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// The service that drafts commit messages.
pub enum Provider {
    /// Google Gemini API (the default).
    #[default]
    Gemini,
    /// OpenRouter, which routes to many model providers.
    OpenRouter,
}

impl Provider {
    /// Every supported provider.
    pub const ALL: [Provider; 2] = [Provider::Gemini, Provider::OpenRouter];

    /// Parse a CLI provider name.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "gemini" | "google" => Ok(Provider::Gemini),
            "openrouter" => Ok(Provider::OpenRouter),
            _ => bail!("expected provider gemini or openrouter"),
        }
    }

    /// Stable identifier used in settings and status JSON.
    pub fn id(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::OpenRouter => "openrouter",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Provider::Gemini => "Gemini",
            Provider::OpenRouter => "OpenRouter",
        }
    }

    /// Model used when none is configured.
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Gemini => DEFAULT_MODEL,
            Provider::OpenRouter => DEFAULT_OPENROUTER_MODEL,
        }
    }

    #[cfg(target_os = "macos")]
    fn keychain_service(self) -> &'static str {
        match self {
            Provider::Gemini => "dev.cooperativ.latch.commit-message.gemini",
            Provider::OpenRouter => "dev.cooperativ.latch.commit-message.openrouter",
        }
    }

    fn valid_model(self, model: &str) -> bool {
        match self {
            Provider::Gemini => valid_gemini_model(model),
            Provider::OpenRouter => valid_openrouter_model(model),
        }
    }
}

/// OpenRouter provider routing. Empty `order` lets OpenRouter choose.
/// See https://openrouter.ai/docs/guides/routing/provider-selection
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Routing {
    /// Provider slugs to try in order; empty lets OpenRouter choose.
    #[serde(default)]
    pub order: Vec<String>,
    /// Whether providers outside `order` may serve the request.
    #[serde(default = "yes")]
    pub allow_fallbacks: bool,
}

impl Default for Routing {
    fn default() -> Self {
        Routing {
            order: Vec::new(),
            allow_fallbacks: true,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenRouterSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default)]
    routing: Routing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    provider: Provider,
    #[serde(default)]
    openrouter: OpenRouterSettings,
}

impl Settings {
    fn validate(&self) -> anyhow::Result<()> {
        if let Some(model) = &self.openrouter.model {
            if !valid_openrouter_model(model) {
                bail!("invalid OpenRouter model");
            }
        }
        validate_routing(&self.openrouter.routing)
    }
}

/// Disable Keychain UI for the entire dedicated gateway process. Access that
/// requires a GUI prompt must report unavailable, never hang an HTTP request.
pub(crate) fn headless_keychain() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let guard =
            security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()
                .map_err(|_| anyhow::anyhow!("cannot disable Keychain interaction"))?;
        // This process remains noninteractive until exit (no restoration).
        std::mem::forget(guard);
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum ConfigError {
    Missing,
    Unavailable,
}

pub(crate) struct ProviderConfig {
    pub provider: Provider,
    pub key: Zeroizing<String>,
    pub model: String,
    pub routing: Routing,
}

pub(crate) fn load(home: &LatchHome) -> Result<ProviderConfig, ConfigError> {
    let settings = settings(home).map_err(|_| ConfigError::Unavailable)?;
    let provider = settings.provider;
    let model = model(home, provider).map_err(|_| ConfigError::Unavailable)?;
    let key = load_key(provider)?;
    Ok(ProviderConfig {
        provider,
        key,
        model,
        routing: settings.openrouter.routing,
    })
}

fn load_key(provider: Provider) -> Result<Zeroizing<String>, ConfigError> {
    let bytes = read_keychain(provider)?;
    let key =
        Zeroizing::new(String::from_utf8(bytes.to_vec()).map_err(|_| ConfigError::Unavailable)?);
    if !valid_key(&key) {
        return Err(ConfigError::Unavailable);
    }
    Ok(key)
}

#[cfg(all(target_os = "macos", not(test)))]
fn read_keychain(provider: Provider) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    security_framework::passwords::get_generic_password(provider.keychain_service(), ACCOUNT)
        .map(Zeroizing::new)
        .map_err(|error| {
            if error.code() == -25300 {
                ConfigError::Missing
            } else {
                ConfigError::Unavailable
            }
        })
}

#[cfg(any(not(target_os = "macos"), test))]
fn read_keychain(_provider: Provider) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    // Tests never use a person's Keychain; no plaintext fallback on other OSes.
    Err(ConfigError::Missing)
}

fn valid_key(key: &str) -> bool {
    (16..=512).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// Gemini models are interpolated into the request URL, so stay strict.
fn valid_gemini_model(model: &str) -> bool {
    (1..=96).contains(&model.len())
        && model.starts_with("gemini-")
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
}

/// OpenRouter models travel in the JSON body as `vendor/model[:variant]`.
fn valid_openrouter_model(model: &str) -> bool {
    (3..=128).contains(&model.len())
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._/:~".contains(&b))
        && model
            .split_once('/')
            .is_some_and(|(vendor, name)| !vendor.is_empty() && !name.is_empty())
        && !model.ends_with('/')
        && !model.contains("//")
        && !model.contains("..")
}

fn valid_routed_provider(slug: &str) -> bool {
    (1..=64).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._/".contains(&b))
        && slug.starts_with(|c: char| c.is_ascii_alphanumeric())
        && !slug.contains("..")
}

fn validate_routing(routing: &Routing) -> anyhow::Result<()> {
    if routing.order.len() > MAX_ROUTED_PROVIDERS {
        bail!("at most {MAX_ROUTED_PROVIDERS} OpenRouter providers may be listed");
    }
    if !routing.order.iter().all(|slug| valid_routed_provider(slug)) {
        bail!("expected OpenRouter provider slugs such as google-vertex or anthropic");
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> anyhow::Result<Option<String>> {
    let mut bytes = Vec::new();
    match std::fs::File::open(path) {
        Ok(file) => {
            file.take(limit).read_to_end(&mut bytes)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("cannot read commit-message configuration"),
    }
    Ok(Some(
        String::from_utf8(bytes).context("invalid commit-message configuration")?,
    ))
}

fn settings(home: &LatchHome) -> anyhow::Result<Settings> {
    let Some(text) = read_bounded(&home.root().join(SETTINGS_FILE), 4096)? else {
        return Ok(Settings::default());
    };
    let settings: Settings =
        serde_json::from_str(&text).context("invalid commit-message settings")?;
    settings.validate()?;
    Ok(settings)
}

fn save_settings(home: &LatchHome, settings: &Settings) -> anyhow::Result<()> {
    settings.validate()?;
    home.ensure()?;
    let mut text = serde_json::to_string_pretty(settings)?;
    text.push('\n');
    write_file(&home.root().join(SETTINGS_FILE), &text)
}

/// The configured model, or `None` when the provider default applies.
fn custom_model(home: &LatchHome, provider: Provider) -> anyhow::Result<Option<String>> {
    match provider {
        Provider::Gemini => {
            let Some(value) = read_bounded(&home.root().join(GEMINI_MODEL_FILE), 128)? else {
                return Ok(None);
            };
            let value = value.trim();
            if !valid_gemini_model(value) {
                bail!("invalid commit-message model");
            }
            Ok(Some(value.into()))
        }
        Provider::OpenRouter => Ok(settings(home)?.openrouter.model),
    }
}

fn model(home: &LatchHome, provider: Provider) -> anyhow::Result<String> {
    Ok(custom_model(home, provider)?.unwrap_or_else(|| provider.default_model().into()))
}

fn set_model(home: &LatchHome, provider: Provider, value: Option<&str>) -> anyhow::Result<()> {
    if let Some(value) = value {
        if !provider.valid_model(value) {
            match provider {
                Provider::Gemini => {
                    bail!("expected a Gemini model identifier such as {DEFAULT_MODEL}")
                }
                Provider::OpenRouter => bail!(
                    "expected an OpenRouter model identifier such as {DEFAULT_OPENROUTER_MODEL}"
                ),
            }
        }
    }
    match provider {
        Provider::Gemini => {
            let path = home.root().join(GEMINI_MODEL_FILE);
            match value {
                Some(value) => {
                    home.ensure()?;
                    write_file(&path, value)
                }
                None => match std::fs::remove_file(path) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        Err(error).context("cannot reset commit-message model")
                    }
                    _ => Ok(()),
                },
            }
        }
        Provider::OpenRouter => {
            let mut settings = settings(home)?;
            settings.openrouter.model = value.map(Into::into);
            save_settings(home, &settings)
        }
    }
}

/// Typed CLI actions, kept independent of Remote Link and Overlord.
pub enum Action {
    /// Store a key for `provider` (default: the active provider).
    SetKey {
        /// `gemini` or `openrouter`.
        provider: Option<String>,
        /// Read one line from stdin instead of a hidden prompt.
        stdin: bool,
    },
    /// Remove the key for `provider` (default: the active provider).
    RemoveKey {
        /// `gemini` or `openrouter`.
        provider: Option<String>,
    },
    /// Report local readiness without contacting a provider.
    Status {
        /// Emit machine-readable JSON.
        json: bool,
    },
    /// Show the active provider, or switch to another.
    Provider {
        /// New active provider.
        value: Option<String>,
    },
    /// Show or set the model for `provider` (default: the active provider).
    Model {
        /// New model identifier.
        value: Option<String>,
        /// `gemini` or `openrouter`.
        provider: Option<String>,
        /// Return to the provider default.
        reset: bool,
    },
    /// Show or set OpenRouter provider routing.
    Routing {
        /// Comma-separated provider slugs; empty clears the order.
        order: Option<String>,
        /// Whether other providers may serve the request.
        allow_fallbacks: Option<bool>,
        /// Return to automatic selection.
        reset: bool,
    },
}

fn target(home: &LatchHome, provider: Option<&str>) -> anyhow::Result<Provider> {
    match provider {
        Some(value) => Provider::parse(value),
        None => Ok(settings(home)?.provider),
    }
}

/// Run one `latch commit-message` action.
pub fn configure(action: Action) -> anyhow::Result<()> {
    let home = LatchHome::from_env()?;
    match action {
        Action::SetKey { provider, stdin } => {
            let provider = target(&home, provider.as_deref())?;
            let key = read_key(stdin, provider)?;
            #[cfg(target_os = "macos")]
            security_framework::passwords::set_generic_password(
                provider.keychain_service(),
                ACCOUNT,
                key.as_bytes(),
            )
            .map_err(|_| anyhow::anyhow!("cannot store {} key in Keychain", provider.label()))?;
            #[cfg(not(target_os = "macos"))]
            {
                drop(key);
                bail!("{} credentials require macOS Keychain", provider.label());
            }
            #[cfg(target_os = "macos")]
            println!("{} credential saved in Keychain.", provider.label());
        }
        Action::RemoveKey { provider } => {
            let provider = target(&home, provider.as_deref())?;
            #[cfg(target_os = "macos")]
            if let Err(error) = security_framework::passwords::delete_generic_password(
                provider.keychain_service(),
                ACCOUNT,
            ) {
                if error.code() != -25300 {
                    bail!("cannot remove {} key from Keychain", provider.label());
                }
            }
            println!("{} credential removed.", provider.label());
        }
        Action::Provider { value } => {
            if let Some(value) = value {
                let mut settings = settings(&home)?;
                settings.provider = Provider::parse(&value)?;
                save_settings(&home, &settings)?;
            }
            println!("{}", settings(&home)?.provider.id());
        }
        Action::Model {
            value,
            provider,
            reset,
        } => {
            let provider = target(&home, provider.as_deref())?;
            if reset || value.is_some() {
                set_model(&home, provider, if reset { None } else { value.as_deref() })?;
            }
            println!("{}", model(&home, provider)?);
        }
        Action::Routing {
            order,
            allow_fallbacks,
            reset,
        } => {
            let mut settings = settings(&home)?;
            if reset {
                settings.openrouter.routing = Routing::default();
            }
            if let Some(order) = order {
                settings.openrouter.routing.order = order
                    .split(',')
                    .map(str::trim)
                    .filter(|slug| !slug.is_empty())
                    .map(Into::into)
                    .collect();
            }
            if let Some(allow) = allow_fallbacks {
                settings.openrouter.routing.allow_fallbacks = allow;
            }
            if reset || settings != self::settings(&home)? {
                save_settings(&home, &settings)?;
            }
            let routing = &settings.openrouter.routing;
            if routing.order.is_empty() {
                println!("automatic (OpenRouter selects the provider)");
            } else {
                println!(
                    "order: {}; fallbacks: {}",
                    routing.order.join(", "),
                    if routing.allow_fallbacks {
                        "allowed"
                    } else {
                        "disabled"
                    }
                );
            }
        }
        Action::Status { json } => {
            headless_keychain()?;
            let state = match load(&home) {
                Ok(_) => "ready",
                Err(ConfigError::Missing) => "unconfigured",
                Err(ConfigError::Unavailable) => "unavailable",
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&status(&home, state))?);
            } else {
                println!(
                    "{}",
                    match state {
                        "ready" => "ready (local configuration only; provider access not verified)",
                        "unconfigured" => "unconfigured",
                        _ => "unavailable (model or Keychain access)",
                    }
                );
            }
        }
    }
    Ok(())
}

/// Machine-readable status for Latch Desktop. Never includes key material.
fn status(home: &LatchHome, state: &str) -> serde_json::Value {
    let settings = settings(home).ok();
    let providers: serde_json::Map<String, serde_json::Value> = Provider::ALL
        .iter()
        .map(|&provider| {
            let custom = custom_model(home, provider);
            let key = match load_key(provider) {
                Ok(_) => "present",
                Err(ConfigError::Missing) => "missing",
                Err(ConfigError::Unavailable) => "unavailable",
            };
            (
                provider.id().into(),
                serde_json::json!({
                    "key": key,
                    "model": custom.as_ref().ok().map(|custom| custom
                        .clone()
                        .unwrap_or_else(|| provider.default_model().into())),
                    "defaultModel": provider.default_model(),
                    "customModel": matches!(custom, Ok(Some(_))),
                }),
            )
        })
        .collect();
    serde_json::json!({
        "state": state,
        "provider": settings.as_ref().map(|s| s.provider.id()),
        "providers": providers,
        "openrouterRouting": settings.as_ref().map(|s| &s.openrouter.routing),
    })
}

fn write_file(path: &Path, contents: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result.context("cannot save commit-message configuration")
}

fn read_key(stdin: bool, provider: Provider) -> anyhow::Result<Zeroizing<String>> {
    use std::os::fd::AsRawFd;
    let mut bytes = Zeroizing::new(Vec::new());
    if stdin {
        std::io::stdin()
            .lock()
            .take(514)
            .read_until(b'\n', &mut bytes)?;
    } else {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .context("no terminal; pipe the credential using --stdin")?;
        let fd = tty.as_raw_fd();
        // Restore echo on every ordinary exit, including a read error.
        struct Echo(i32, libc::termios);
        impl Drop for Echo {
            fn drop(&mut self) {
                unsafe {
                    libc::tcsetattr(self.0, libc::TCSANOW, &self.1);
                }
            }
        }
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            bail!("cannot secure terminal input");
        }
        let original = unsafe { original.assume_init() };
        let _echo = Echo(fd, original);
        let mut hidden = original;
        // Read control keys ourselves so Ctrl-C restores echo before returning.
        hidden.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG);
        hidden.c_cc[libc::VMIN] = 1;
        hidden.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } != 0 {
            bail!("cannot disable terminal echo");
        }
        eprint!("{} API key: ", provider.label());
        std::io::stderr().flush()?;
        loop {
            let mut byte = [0];
            tty.read_exact(&mut byte)?;
            match byte[0] {
                b'\r' | b'\n' => break,
                3 | 4 => {
                    eprintln!();
                    bail!("credential entry cancelled");
                }
                8 | 127 => {
                    bytes.pop();
                }
                value => bytes.push(value),
            }
            if bytes.len() > 512 {
                bail!("credential is too long");
            }
        }
        eprintln!();
    }
    let key = Zeroizing::new(
        String::from_utf8(bytes.to_vec())
            .map_err(|_| anyhow::anyhow!("invalid credential format"))?,
    );
    let key = Zeroizing::new(key.trim_end_matches(['\r', '\n']).to_owned());
    if !valid_key(&key) {
        bail!("invalid credential format");
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_configuration_reloads_model_without_mobile_or_gateway_restart() {
        let dir = tempfile::tempdir().unwrap();
        let home = LatchHome::new(dir.path().to_owned());
        assert_eq!(
            model(&home, Provider::Gemini).unwrap(),
            "gemini-3.5-flash-lite"
        );
        let path = home.root().join(GEMINI_MODEL_FILE);
        write_file(&path, "gemini-supported-override").unwrap();
        assert_eq!(
            model(&home, Provider::Gemini).unwrap(),
            "gemini-supported-override"
        );
        write_file(&path, DEFAULT_MODEL).unwrap();
        assert_eq!(model(&home, Provider::Gemini).unwrap(), DEFAULT_MODEL);
        for value in [
            "",
            "https://example.invalid/key",
            "gemini-a/b",
            "gemini-a?key=secret",
            "gemini-a\nheader",
            "../gemini-a",
        ] {
            assert!(!valid_gemini_model(value));
            std::fs::write(&path, value).unwrap();
            assert!(model(&home, Provider::Gemini).is_err());
        }
        for key in [
            "",
            "short",
            "a key with spaces",
            "abcdefghijklmnop\n",
            "abcdefghijklmnop:header",
        ] {
            assert!(!valid_key(key));
        }
        assert!(valid_key("fixture_only_key_1234"));
        assert!(valid_key("AQ.fixture_key-123456789"));
        // A real OpenRouter key is this prefix plus 64 hex characters. Build it
        // so that shape is tested without a contiguous literal push protection
        // treats as a leaked key.
        let openrouter_shaped = format!(
            "{prefix}{body}",
            prefix = "sk-or-v1-",
            body = "0123456789abcdef".repeat(4)
        );
        assert!(valid_key(&openrouter_shaped));
        assert!(!valid_key(&"a".repeat(513)));
        // Unit tests must never discover a real Keychain credential.
        for provider in Provider::ALL {
            assert!(matches!(load_key(provider), Err(ConfigError::Missing)));
        }
    }

    #[test]
    fn openrouter_settings_default_to_gemini_and_validate_model_and_routing() {
        let dir = tempfile::tempdir().unwrap();
        let home = LatchHome::new(dir.path().to_owned());
        home.ensure().unwrap();
        // No settings file: existing Gemini setups are unchanged.
        assert_eq!(settings(&home).unwrap().provider, Provider::Gemini);
        assert_eq!(
            model(&home, Provider::OpenRouter).unwrap(),
            DEFAULT_OPENROUTER_MODEL
        );

        let mut value = settings(&home).unwrap();
        value.provider = Provider::OpenRouter;
        save_settings(&home, &value).unwrap();
        set_model(
            &home,
            Provider::OpenRouter,
            Some("anthropic/claude-haiku-5.5"),
        )
        .unwrap();
        assert_eq!(
            model(&home, Provider::OpenRouter).unwrap(),
            "anthropic/claude-haiku-5.5"
        );
        // Changing the OpenRouter model leaves the provider choice intact.
        assert_eq!(settings(&home).unwrap().provider, Provider::OpenRouter);
        set_model(&home, Provider::OpenRouter, None).unwrap();
        assert_eq!(
            model(&home, Provider::OpenRouter).unwrap(),
            DEFAULT_OPENROUTER_MODEL
        );
        set_model(&home, Provider::Gemini, Some("gemini-custom")).unwrap();
        set_model(&home, Provider::Gemini, None).unwrap();
        assert_eq!(model(&home, Provider::Gemini).unwrap(), DEFAULT_MODEL);

        for valid in [
            "google/gemini-3.5-flash-lite",
            "openai/gpt-5:free",
            "openrouter/auto",
            "~anthropic/claude-sonnet-latest",
            "meta-llama/llama-3.1-8b-instruct",
        ] {
            assert!(valid_openrouter_model(valid), "{valid}");
        }
        for invalid in [
            "",
            "gemini-3.5-flash-lite",
            "/model",
            "vendor/",
            "a//b",
            "vendor/../x",
            "vendor/model name",
            "vendor/model\n",
            "vendor/model\"",
        ] {
            assert!(!valid_openrouter_model(invalid), "{invalid:?}");
            assert!(set_model(&home, Provider::OpenRouter, Some(invalid)).is_err());
        }
        // Gemini identifiers are rejected for OpenRouter and vice versa.
        assert!(set_model(&home, Provider::Gemini, Some("google/gemini-x")).is_err());

        let mut value = settings(&home).unwrap();
        value.openrouter.routing = Routing {
            order: vec!["google-vertex".into(), "google-ai-studio".into()],
            allow_fallbacks: false,
        };
        save_settings(&home, &value).unwrap();
        assert_eq!(settings(&home).unwrap(), value);
        for order in [
            vec!["bad slug".to_string()],
            vec!["../etc".into()],
            vec!["".into()],
            vec!["a".into(); MAX_ROUTED_PROVIDERS + 1],
        ] {
            value.openrouter.routing.order = order;
            assert!(save_settings(&home, &value).is_err());
        }

        // A tampered settings file fails closed instead of guessing.
        let path = home.root().join(SETTINGS_FILE);
        std::fs::write(&path, r#"{"provider":"openrouter","extra":1}"#).unwrap();
        assert!(settings(&home).is_err());
        assert!(matches!(load(&home), Err(ConfigError::Unavailable)));
        std::fs::write(
            &path,
            r#"{"provider":"openrouter","openrouter":{"model":"x"}}"#,
        )
        .unwrap();
        assert!(settings(&home).is_err());
        std::fs::write(&path, r#"{"provider":"openrouter"}"#).unwrap();
        assert_eq!(
            settings(&home).unwrap().openrouter.routing,
            Routing::default()
        );
        assert!(matches!(load(&home), Err(ConfigError::Missing)));
    }
}
