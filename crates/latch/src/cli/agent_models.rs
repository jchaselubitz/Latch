//! Models a hosted agent can start with, as the agent itself lists them on
//! this Mac.
//!
//! Neither Claude Code nor Codex has a stable "list models" command, but both
//! keep the list they last fetched from their service on disk and refresh it
//! as they run: Claude Code under `~/.claude/cache/model-catalog/`, Codex in
//! `~/.codex/models_cache.json`. Reading those keeps the phone's picker as
//! current as the CLIs themselves, without Latch shipping a new list for every
//! model release. Each cache is the agent's private format, so anything
//! missing or unreadable falls back to the list bundled here, and every id is
//! held to the contract's shape before it is offered or launched.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::cli::serve::{AgentModel, AgentModelCatalog, SessionAgent};

/// Most models one catalog offers; the contract's bound.
const MAX_MODELS: usize = 32;
/// Largest cache file read. Both are around 10 KiB; anything far larger is
/// not the file this code understands.
const MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 160;

/// The catalog for `agent`, read fresh from the owner's agent caches.
pub fn catalog(agent: SessionAgent) -> AgentModelCatalog {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    catalog_in(agent, home.as_deref(), codex_home.as_deref())
}

/// Whether `value` has the shape the contract allows for a model id: what the
/// agents' own ids and aliases look like (`claude-opus-5-5`, `gpt-6.1-sol`,
/// `opus[1m]`), and never something an argument parser could read as a flag.
pub fn is_model_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_ID_BYTES
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/@[]-".contains(byte))
}

fn catalog_in(
    agent: SessionAgent,
    home: Option<&Path>,
    codex_home: Option<&Path>,
) -> AgentModelCatalog {
    let (cached, default_model) = match agent {
        SessionAgent::Claude => {
            let root = home.map(|home| home.join(".claude"));
            let root = root.as_deref();
            (
                root.and_then(claude_cached_models),
                root.and_then(claude_default_model),
            )
        }
        SessionAgent::Codex => {
            let root = codex_home
                .map(Path::to_owned)
                .or_else(|| home.map(|home| home.join(".codex")));
            let root = root.as_deref();
            (
                root.and_then(codex_cached_models),
                root.and_then(codex_default_model),
            )
        }
    };
    AgentModelCatalog {
        agent,
        models: cached
            .filter(|models| !models.is_empty())
            .unwrap_or_else(|| bundled(agent)),
        default_model,
    }
}

/// Claude Code's catalog for the terminal surface (`"cc"`), newest file
/// first. The directory holds one file per account and surface; the desktop
/// surface (`"ccd"`) lists the same models and is used only when no terminal
/// catalog exists. Only the `main` section is offered — the models Claude
/// Code's own picker leads with — unless the file marks none that way.
fn claude_cached_models(root: &Path) -> Option<Vec<AgentModel>> {
    let mut files: Vec<(SystemTime, PathBuf)> = fs::read_dir(root.join("cache/model-catalog"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let modified = fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

    let mut fallback = None;
    for (_, path) in files {
        let Some(document) = read_json(&path) else {
            continue;
        };
        let catalog = &document["catalog"];
        let Some(models) = catalog["config"]["models"].as_array() else {
            continue;
        };
        let main: Vec<&Value> = models
            .iter()
            .filter(|model| model["section"].as_str() == Some("main"))
            .collect();
        let chosen = if main.is_empty() {
            models.iter().collect()
        } else {
            main
        };
        let parsed = collect_models(chosen.into_iter().map(|model| {
            (
                model["id"].as_str(),
                model["name"].as_str(),
                model["description"].as_str(),
            )
        }));
        if parsed.is_empty() {
            continue;
        }
        if catalog["surface"].as_str() == Some("cc") {
            return Some(parsed);
        }
        fallback.get_or_insert(parsed);
    }
    fallback
}

/// The model the owner set in Claude Code's user settings.
fn claude_default_model(root: &Path) -> Option<String> {
    let settings = read_json(&root.join("settings.json"))?;
    settings["model"]
        .as_str()
        .map(str::trim)
        .filter(|model| is_model_id(model))
        .map(str::to_owned)
}

/// Codex's model cache: the models it lists in its own picker
/// (`visibility: "list"`), in its own priority order.
fn codex_cached_models(root: &Path) -> Option<Vec<AgentModel>> {
    let document = read_json(&root.join("models_cache.json"))?;
    let mut models: Vec<&Value> = document["models"]
        .as_array()?
        .iter()
        .filter(|model| model["visibility"].as_str().is_none_or(|v| v == "list"))
        .collect();
    models.sort_by_key(|model| model["priority"].as_i64().unwrap_or(i64::MAX));
    Some(collect_models(models.into_iter().map(|model| {
        (
            model["slug"].as_str(),
            model["display_name"].as_str(),
            model["description"].as_str(),
        )
    })))
}

/// The top-level `model = "…"` in Codex's `config.toml`. Only the root table
/// is read: a `model` key under `[profiles.x]` is not the default.
fn codex_default_model(root: &Path) -> Option<String> {
    let path = root.join("config.toml");
    if fs::metadata(&path).ok()?.len() > MAX_CACHE_BYTES {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some(rest) = line.strip_prefix("model") else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim();
        let quote = value.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let inner = &value[1..];
        let end = inner.find(quote)?;
        let model = &inner[..end];
        return is_model_id(model).then(|| model.to_owned());
    }
    None
}

fn collect_models<'a>(
    entries: impl Iterator<Item = (Option<&'a str>, Option<&'a str>, Option<&'a str>)>,
) -> Vec<AgentModel> {
    let mut models: Vec<AgentModel> = Vec::new();
    for (id, name, description) in entries {
        let Some(id) = id.map(str::trim).filter(|id| is_model_id(id)) else {
            continue;
        };
        if models.iter().any(|model| model.id == id) {
            continue;
        }
        let name = name
            .map(|name| display_text(name, MAX_NAME_CHARS))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.to_owned());
        let description = description
            .map(|text| display_text(text, MAX_DESCRIPTION_CHARS))
            .filter(|text| !text.is_empty());
        models.push(AgentModel {
            id: id.to_owned(),
            name,
            description,
        });
        if models.len() == MAX_MODELS {
            break;
        }
    }
    models
}

/// One line of display text: control characters dropped, whitespace folded,
/// and bounded in characters rather than bytes.
fn display_text(value: &str, max_chars: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|ch| !ch.is_control())
        .take(max_chars)
        .collect()
}

fn read_json(path: &Path) -> Option<Value> {
    if fs::metadata(path).ok()?.len() > MAX_CACHE_BYTES {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Shipped for a Mac whose agent has never run or whose cache this build
/// cannot read. Current as of this release; the live caches supersede it.
fn bundled(agent: SessionAgent) -> Vec<AgentModel> {
    let models: &[(&str, &str)] = match agent {
        SessionAgent::Claude => &[
            ("claude-opus-5-5", "Opus 5.5"),
            ("claude-fable-5-1", "Fable 5.1"),
            ("claude-sonnet-5-5", "Sonnet 5.5"),
            ("claude-haiku-4-5-20251001", "Haiku 4.5"),
        ],
        SessionAgent::Codex => &[
            ("gpt-6.1-sol", "GPT-6.1-Sol"),
            ("gpt-6-astra", "GPT-6-Astra"),
            ("gpt-6-sol", "GPT-6-Sol"),
            ("gpt-6-luna", "GPT-6-Luna"),
            ("gpt-5.6-sol", "GPT-5.6-Sol"),
            ("gpt-5.6-terra", "GPT-5.6-Terra"),
            ("gpt-5.6-luna", "GPT-5.6-Luna"),
        ],
    };
    models
        .iter()
        .map(|(id, name)| AgentModel {
            id: (*id).to_owned(),
            name: (*name).to_owned(),
            description: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ids(catalog: &AgentModelCatalog) -> Vec<&str> {
        catalog
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect()
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn age(path: &Path, seconds_ago: u64) {
        let when = SystemTime::now() - Duration::from_secs(seconds_ago);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn claude_catalog(surface: &str, models: &str) -> String {
        format!(
            r#"{{"version":2,"catalog":{{"surface":"{surface}","config":{{"id":"{surface}","models":{models}}}}}}}"#
        )
    }

    #[test]
    fn model_ids_are_the_agents_shapes_and_never_a_flag() {
        for id in [
            "claude-opus-5-5",
            "gpt-6.1-sol",
            "opus",
            "opus[1m]",
            "openai/gpt-5",
        ] {
            assert!(is_model_id(id), "{id}");
        }
        for id in [
            "",
            "-x",
            "--dangerously-skip-permissions",
            "a b",
            "a;b",
            "a\nb",
            "ä",
        ] {
            assert!(!is_model_id(id), "{id:?}");
        }
        assert!(!is_model_id(&"a".repeat(MAX_ID_BYTES + 1)));
    }

    #[test]
    fn claude_reads_the_newest_terminal_catalog_and_its_main_section() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude/cache/model-catalog");
        let old = dir.join("old-cc.json");
        write(
            &old,
            &claude_catalog(
                "cc",
                r#"[{"id":"claude-old-1","name":"Old","section":"main"}]"#,
            ),
        );
        age(&old, 600);
        // Newer, but the desktop surface: only used without a terminal file.
        write(
            &dir.join("acct-ccd.json"),
            &claude_catalog(
                "ccd",
                r#"[{"id":"claude-desk-1","name":"Desk","section":"main"}]"#,
            ),
        );
        let current = dir.join("acct-cc.json");
        write(
            &current,
            &claude_catalog(
                "cc",
                r#"[
                  {"id":"claude-opus-5-5","name":"Opus 5.5","description":"For complex work","section":"main"},
                  {"id":"claude-fable-5-1","name":"Fable 5.1","section":"main"},
                  {"id":"--bad","name":"Bad","section":"main"},
                  {"id":"claude-opus-4-8","name":"Opus 4.8","section":"overflow"}
                ]"#,
            ),
        );
        age(&current, 60);
        write(
            &home.path().join(".claude/settings.json"),
            r#"{"model":"claude-fable-5-1"}"#,
        );

        let catalog = catalog_in(SessionAgent::Claude, Some(home.path()), None);
        assert_eq!(ids(&catalog), ["claude-opus-5-5", "claude-fable-5-1"]);
        assert_eq!(
            catalog.models[0].description.as_deref(),
            Some("For complex work")
        );
        assert_eq!(catalog.default_model.as_deref(), Some("claude-fable-5-1"));
    }

    #[test]
    fn claude_without_a_terminal_catalog_uses_the_desktop_one() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home
                .path()
                .join(".claude/cache/model-catalog/acct-ccd.json"),
            &claude_catalog("ccd", r#"[{"id":"claude-desk-1","name":"Desk"}]"#),
        );
        let catalog = catalog_in(SessionAgent::Claude, Some(home.path()), None);
        assert_eq!(ids(&catalog), ["claude-desk-1"]);
        assert_eq!(catalog.default_model, None);
    }

    #[test]
    fn codex_lists_its_visible_models_in_priority_order() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        write(
            &codex.join("models_cache.json"),
            r#"{"models":[
              {"slug":"gpt-6-sol","display_name":"GPT-6-Sol","visibility":"list","priority":3},
              {"slug":"gpt-reserve","display_name":"Reserve","visibility":"hide","priority":1},
              {"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","description":"Latest","visibility":"list","priority":1}
            ]}"#,
        );
        write(
            &codex.join("config.toml"),
            "# comment\nmodel_reasoning_effort = \"medium\"\nmodel = \"gpt-6.1-sol\"\n\n[profiles.fast]\nmodel = \"gpt-6-luna\"\n",
        );
        let catalog = catalog_in(SessionAgent::Codex, Some(home.path()), None);
        assert_eq!(ids(&catalog), ["gpt-6.1-sol", "gpt-6-sol"]);
        assert_eq!(catalog.models[0].description.as_deref(), Some("Latest"));
        assert_eq!(catalog.default_model.as_deref(), Some("gpt-6.1-sol"));
    }

    #[test]
    fn codex_home_overrides_the_home_directory() {
        let home = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        write(
            &codex_home.path().join("models_cache.json"),
            r#"{"models":[{"slug":"gpt-elsewhere","display_name":"Elsewhere","visibility":"list","priority":1}]}"#,
        );
        let catalog = catalog_in(
            SessionAgent::Codex,
            Some(home.path()),
            Some(codex_home.path()),
        );
        assert_eq!(ids(&catalog), ["gpt-elsewhere"]);
    }

    #[test]
    fn a_profile_model_is_not_the_codex_default() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".codex/config.toml"),
            "[profiles.fast]\nmodel = \"gpt-6-luna\"\n",
        );
        let catalog = catalog_in(SessionAgent::Codex, Some(home.path()), None);
        assert_eq!(catalog.default_model, None);
    }

    #[test]
    fn missing_or_unreadable_caches_fall_back_to_the_bundled_lists() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join(".codex/models_cache.json"), "not json");
        write(
            &home.path().join(".claude/cache/model-catalog/x-cc.json"),
            &claude_catalog("cc", "[]"),
        );
        for agent in [SessionAgent::Claude, SessionAgent::Codex] {
            let catalog = catalog_in(agent, Some(home.path()), None);
            assert_eq!(catalog.models, bundled(agent));
            assert!(catalog.models.iter().all(|model| is_model_id(&model.id)));
        }
        let none = catalog_in(SessionAgent::Claude, None, None);
        assert_eq!(none.models, bundled(SessionAgent::Claude));
    }

    #[test]
    fn display_text_is_one_bounded_line() {
        assert_eq!(display_text("  Opus\n 5.5\t ", 64), "Opus 5.5");
        assert_eq!(display_text("abcdef", 3), "abc");
    }
}
