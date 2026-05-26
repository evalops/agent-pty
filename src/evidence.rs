use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{LazyLock, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

static EVENT_LOG_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create event log directory {}", parent.display()))?;
        }
        Ok(Self { path })
    }

    pub fn append_action(
        &self,
        session_id: impl Into<String>,
        action: Action,
        git_snapshot: Option<GitSnapshot>,
    ) -> Result<EvidenceEvent> {
        self.append(EvidenceEvent::new(
            session_id.into(),
            EventKind::Action(action),
            git_snapshot,
        ))
    }

    pub fn append_observation(
        &self,
        session_id: impl Into<String>,
        observation: Observation,
    ) -> Result<EvidenceEvent> {
        self.append(EvidenceEvent::new(
            session_id.into(),
            EventKind::Observation(observation),
            None,
        ))
    }

    pub fn replay(&self) -> Result<Vec<EvidenceEvent>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .with_context(|| format!("open event log {}", self.path.display()))?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for (index, line) in reader.lines().enumerate() {
            let line =
                line.with_context(|| format!("read event log line {}", index.saturating_add(1)))?;
            if line.trim().is_empty() {
                continue;
            }
            let event = serde_json::from_str(&line).with_context(|| {
                format!(
                    "parse event log line {} from {}",
                    index.saturating_add(1),
                    self.path.display()
                )
            })?;
            events.push(event);
        }

        Ok(events)
    }

    fn append(&self, event: EvidenceEvent) -> Result<EvidenceEvent> {
        let _guard = EVENT_LOG_WRITE_LOCK
            .lock()
            .expect("event log write lock poisoned");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open event log {}", self.path.display()))?;
        serde_json::to_writer(&mut file, &event)
            .with_context(|| format!("serialize event for {}", event.session_id))?;
        file.write_all(b"\n")
            .with_context(|| format!("write event log {}", self.path.display()))?;
        Ok(event)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceEvent {
    pub id: Uuid,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub kind: EventKind,
    pub git_snapshot: Option<GitSnapshot>,
}

impl EvidenceEvent {
    pub fn new(
        session_id: String,
        kind: EventKind,
        git_snapshot: Option<GitSnapshot>,
    ) -> EvidenceEvent {
        EvidenceEvent {
            id: Uuid::new_v4(),
            session_id,
            timestamp: Utc::now(),
            kind,
            git_snapshot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "payload", rename_all = "snake_case")]
pub enum EventKind {
    Action(Action),
    Observation(Observation),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    SendKeys {
        bytes: Vec<u8>,
    },
    Exec {
        argv: Vec<String>,
        cwd: PathBuf,
    },
    WaitFor {
        predicate: Predicate,
        timeout: Duration,
    },
    SnapshotScreen,
    SnapshotFiles {
        globs: Vec<String>,
    },
    Kill {
        signal: String,
    },
    AttachHuman,
    PolicyDenied {
        command: String,
        rule: String,
    },
    Fork {
        source: String,
        target: String,
        workspace: PathBuf,
    },
    Proof {
        json_path: PathBuf,
        markdown_path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Predicate {
    Regex { pattern: String },
    Prompt,
    Exit,
    Idle { quiet_for: Duration },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub screen_text: Option<String>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub exit_status: Option<i32>,
    pub files_changed: Vec<FileChange>,
    pub git_snapshot: Option<GitSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: PathBuf,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSnapshot {
    pub workspace: PathBuf,
    pub head: Option<String>,
    pub clean: bool,
    pub status_short: String,
    pub diff: String,
    pub staged_diff: String,
}

impl GitSnapshot {
    pub fn capture(workspace: impl AsRef<Path>) -> Result<Self> {
        let workspace = workspace.as_ref().to_path_buf();
        ensure_git_worktree(&workspace)?;

        let head = git_output(&workspace, &["rev-parse", "--short", "HEAD"])
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let status_short = git_output(&workspace, &["status", "--short"])?;
        let diff = git_output(&workspace, &["diff", "--no-ext-diff", "--"])?;
        let staged_diff = git_output(&workspace, &["diff", "--no-ext-diff", "--cached", "--"])?;

        Ok(Self {
            workspace,
            head,
            clean: status_short.trim().is_empty(),
            status_short,
            diff,
            staged_diff,
        })
    }
}

fn ensure_git_worktree(workspace: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace)
        .output()
        .with_context(|| format!("run git in {}", workspace.display()))?;

    if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true" {
        Ok(())
    } else {
        anyhow::bail!("{} is not a git worktree", workspace.display());
    }
}

fn git_output(workspace: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .with_context(|| format!("run git {args:?} in {}", workspace.display()))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        anyhow::bail!(
            "git {args:?} failed in {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
}
