use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use alleycat_bridge_core::Hydrator;
pub use alleycat_bridge_core::{
    IndexEntry as CoreIndexEntry, ListFilter, ListPage, ListSort, ThreadIndex as CoreThreadIndex,
};
use alleycat_codex_proto::{SessionSource, Thread, ThreadSourceKind, ThreadStatus};

pub const CLI_VERSION: &str = concat!("alleycat-amp-bridge/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmpSessionRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amp_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amp_thread_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

pub type IndexEntry = CoreIndexEntry<AmpSessionRef>;

pub fn entry_to_thread(entry: &IndexEntry) -> Thread {
    Thread {
        id: entry.thread_id.clone(),
        session_id: entry
            .metadata
            .amp_thread_id
            .clone()
            .unwrap_or_else(|| entry.thread_id.clone()),
        forked_from_id: entry.forked_from_id.clone(),
        preview: entry.preview.clone(),
        ephemeral: false,
        model_provider: entry.model_provider.clone(),
        created_at: entry.created_at,
        updated_at: entry.updated_at,
        status: ThreadStatus::NotLoaded,
        path: entry
            .metadata
            .amp_thread_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        cwd: entry.cwd.clone(),
        cli_version: CLI_VERSION.to_string(),
        source: source_kind_to_session_source(entry.source),
        thread_source: None,
        agent_nickname: None,
        agent_role: None,
        git_info: alleycat_bridge_core::git_info_for_cwd(&entry.cwd),
        name: entry.name.clone(),
        turns: Vec::new(),
    }
}

fn source_kind_to_session_source(kind: ThreadSourceKind) -> SessionSource {
    match kind {
        ThreadSourceKind::Cli => SessionSource::Cli,
        ThreadSourceKind::VsCode => SessionSource::VsCode,
        ThreadSourceKind::Exec => SessionSource::Exec,
        ThreadSourceKind::AppServer => SessionSource::AppServer,
        _ => SessionSource::AppServer,
    }
}

pub struct AmpHydrator {
    override_dir: Option<PathBuf>,
}

impl AmpHydrator {
    pub fn new() -> Self {
        Self { override_dir: None }
    }

    pub fn with_override_dir(dir: PathBuf) -> Self {
        Self {
            override_dir: Some(dir),
        }
    }
}

impl Default for AmpHydrator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Hydrator<AmpSessionRef> for AmpHydrator {
    async fn scan(&self) -> Result<Vec<IndexEntry>> {
        let Some(dir) = self.override_dir.clone().or_else(amp_threads_dir) else {
            return Ok(Vec::new());
        };
        Ok(scan_thread_dir(&dir).await)
    }
}

pub async fn open_and_hydrate(codex_home: &Path) -> Result<Arc<CoreThreadIndex<AmpSessionRef>>> {
    CoreThreadIndex::open_and_hydrate(codex_home.join("threads.json"), &AmpHydrator::new()).await
}

pub fn amp_threads_dir() -> Option<PathBuf> {
    if let Ok(env_dir) = std::env::var("AMP_THREADS_DIR") {
        return Some(expand_tilde(&env_dir));
    }
    let home = directories::UserDirs::new()?.home_dir().to_path_buf();
    Some(
        home.join(".local")
            .join("share")
            .join("amp")
            .join("threads"),
    )
}

fn expand_tilde(input: &str) -> PathBuf {
    if input == "~" {
        if let Some(home) = directories::UserDirs::new() {
            return home.home_dir().to_path_buf();
        }
    }
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = directories::UserDirs::new() {
            return home.home_dir().join(rest);
        }
    }
    PathBuf::from(input)
}

async fn scan_thread_dir(dir: &Path) -> Vec<IndexEntry> {
    let mut out = Vec::new();
    let mut read_dir = match tokio::fs::read_dir(dir).await {
        Ok(read_dir) => read_dir,
        Err(_) => return out,
    };
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Some(row) = entry_from_thread_file(&path).await {
            out.push(row);
        }
    }
    out
}

async fn entry_from_thread_file(path: &Path) -> Option<IndexEntry> {
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
    let now = Utc::now().timestamp_millis();
    let created_at = value
        .get("created")
        .or_else(|| value.get("createdAt"))
        .and_then(value_to_millis)
        .unwrap_or(now);
    let updated_at = value
        .get("updatedAt")
        .or_else(|| value.get("updated"))
        .and_then(value_to_millis)
        .unwrap_or(created_at);
    let name = value
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    let preview = first_message_preview(&value)
        .or_else(|| name.clone())
        .unwrap_or_else(|| "(no messages)".to_string());
    let cwd = value
        .get("cwd")
        .or_else(|| value.get("workingDirectory"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(IndexEntry {
        thread_id: id.clone(),
        cwd,
        name,
        preview,
        created_at,
        updated_at,
        archived: false,
        forked_from_id: None,
        model_provider: "amp".to_string(),
        source: ThreadSourceKind::AppServer,
        metadata: AmpSessionRef {
            amp_thread_id: Some(id),
            amp_thread_path: Some(path.to_path_buf()),
            model: None,
            reasoning_effort: None,
        },
    })
}

fn value_to_millis(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(if n < 10_000_000_000 { n * 1000 } else { n });
    }
    let s = value.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn first_message_preview(value: &Value) -> Option<String> {
    let messages = value.get("messages")?.as_array()?;
    for message in messages {
        let content = message
            .get("content")
            .or_else(|| message.get("message").and_then(|m| m.get("content")))?;
        if let Some(text) = content.as_str() {
            let text = text.lines().next().unwrap_or("").trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
        if let Some(blocks) = content.as_array() {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = block.get("text").and_then(Value::as_str)
                {
                    let text = text.lines().next().unwrap_or("").trim();
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
        }
    }
    None
}
