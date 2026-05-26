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
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

static EVENT_LOG_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static REDACTION_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(
                r#"(?i)\b([A-Z0-9_]*(?:TOKEN|SECRET|PASSWORD|API_KEY|ACCESS_KEY)[A-Z0-9_]*)=([^\s'";]+)"#,
            )
            .expect("valid env secret redaction regex"),
            "$1=[REDACTED]",
        ),
        (
            Regex::new(r"sk-[A-Za-z0-9_-]{16,}").expect("valid OpenAI key redaction regex"),
            "sk-[REDACTED]",
        ),
        (
            Regex::new(r"gh[pousr]_[A-Za-z0-9_]{20,}")
                .expect("valid GitHub token redaction regex"),
            "gh_[REDACTED]",
        ),
        (
            Regex::new(r"AKIA[0-9A-Z]{16}").expect("valid AWS access key redaction regex"),
            "AKIA[REDACTED]",
        ),
    ]
});

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
        fs2::FileExt::lock_shared(&file)
            .with_context(|| format!("lock event log {}", self.path.display()))?;
        read_events_from_file(file, &self.path)
    }

    pub fn verify_integrity(&self) -> Result<LogIntegrity> {
        let events = self.replay()?;
        let mut previous_hash = None;
        for (index, event) in events.iter().enumerate() {
            let expected_sequence = index as u64 + 1;
            if event.sequence != expected_sequence {
                return Ok(LogIntegrity::failed(
                    events.len(),
                    format!(
                        "event {} has sequence {}, expected {}",
                        index.saturating_add(1),
                        event.sequence,
                        expected_sequence
                    ),
                ));
            }
            if event.previous_hash != previous_hash {
                return Ok(LogIntegrity::failed(
                    events.len(),
                    format!(
                        "event {} previous hash does not match",
                        index.saturating_add(1)
                    ),
                ));
            }
            let expected_hash = event_hash(event)?;
            if event.event_hash != expected_hash {
                return Ok(LogIntegrity::failed(
                    events.len(),
                    format!("event {} hash does not match", index.saturating_add(1)),
                ));
            }
            previous_hash = Some(event.event_hash.clone());
        }

        Ok(LogIntegrity {
            verified: true,
            event_count: events.len(),
            head_hash: previous_hash,
            first_error: None,
        })
    }

    fn append(&self, event: EvidenceEvent) -> Result<EvidenceEvent> {
        let _guard = EVENT_LOG_WRITE_LOCK
            .lock()
            .expect("event log write lock poisoned");
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open event log {}", self.path.display()))?;
        fs2::FileExt::lock_exclusive(&file)
            .with_context(|| format!("lock event log {}", self.path.display()))?;
        let existing_events = read_events_from_file(
            file.try_clone()
                .with_context(|| format!("clone event log {}", self.path.display()))?,
            &self.path,
        )?;
        let (sequence, previous_hash) = next_integrity_fields(&existing_events);
        let mut event = redact_event(event);
        event.sequence = sequence;
        event.previous_hash = previous_hash;
        event.event_hash = event_hash(&event)?;
        let mut line = serde_json::to_vec(&event)
            .with_context(|| format!("serialize event for {}", event.session_id))?;
        line.push(b'\n');
        file.write_all(&line)
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
    #[serde(default)]
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_hash: Option<String>,
    #[serde(default)]
    pub event_hash: String,
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
            sequence: 0,
            previous_hash: None,
            event_hash: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogIntegrity {
    pub verified: bool,
    pub event_count: usize,
    pub head_hash: Option<String>,
    pub first_error: Option<String>,
}

impl LogIntegrity {
    fn failed(event_count: usize, first_error: String) -> Self {
        Self {
            verified: false,
            event_count,
            head_hash: None,
            first_error: Some(first_error),
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
    PolicyApprovalCreated {
        command: String,
        rule: String,
        approval_id: String,
        expires_at: DateTime<Utc>,
    },
    PolicyApproved {
        command: String,
        rule: String,
        approval_id: String,
    },
    Fork {
        source: String,
        target: String,
        workspace: PathBuf,
    },
    Proof {
        json_path: PathBuf,
        markdown_path: PathBuf,
        html_path: PathBuf,
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

fn read_events_from_file(file: fs::File, path: &Path) -> Result<Vec<EvidenceEvent>> {
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
                path.display()
            )
        })?;
        events.push(event);
    }

    Ok(events)
}

fn next_integrity_fields(events: &[EvidenceEvent]) -> (u64, Option<String>) {
    let mut sequence = 1;
    let mut previous_hash = None;
    for event in events {
        sequence = event.sequence.saturating_add(1).max(sequence);
        if !event.event_hash.is_empty() {
            previous_hash = Some(event.event_hash.clone());
        }
    }
    (sequence, previous_hash)
}

pub fn redact_text(value: &str) -> String {
    let mut redacted = value.to_string();
    for (pattern, replacement) in REDACTION_PATTERNS.iter() {
        redacted = pattern.replace_all(&redacted, *replacement).into_owned();
    }
    redacted
}

fn redact_event(mut event: EvidenceEvent) -> EvidenceEvent {
    event.kind = redact_kind(event.kind);
    event.git_snapshot = event.git_snapshot.map(redact_git_snapshot);
    event
}

fn redact_kind(kind: EventKind) -> EventKind {
    match kind {
        EventKind::Action(action) => EventKind::Action(redact_action(action)),
        EventKind::Observation(observation) => {
            EventKind::Observation(redact_observation(observation))
        }
    }
}

fn redact_action(action: Action) -> Action {
    match action {
        Action::SendKeys { bytes } => Action::SendKeys {
            bytes: redact_text(&String::from_utf8_lossy(&bytes)).into_bytes(),
        },
        Action::Exec { argv, cwd } => Action::Exec {
            argv: argv.into_iter().map(|value| redact_text(&value)).collect(),
            cwd,
        },
        Action::PolicyDenied { command, rule } => Action::PolicyDenied {
            command: redact_text(&command),
            rule,
        },
        Action::PolicyApprovalCreated {
            command,
            rule,
            approval_id,
            expires_at,
        } => Action::PolicyApprovalCreated {
            command: redact_text(&command),
            rule,
            approval_id,
            expires_at,
        },
        Action::PolicyApproved {
            command,
            rule,
            approval_id,
        } => Action::PolicyApproved {
            command: redact_text(&command),
            rule,
            approval_id,
        },
        other => other,
    }
}

fn redact_observation(observation: Observation) -> Observation {
    Observation {
        screen_text: observation.screen_text.map(|value| redact_text(&value)),
        stdout_tail: redact_text(&observation.stdout_tail),
        stderr_tail: redact_text(&observation.stderr_tail),
        exit_status: observation.exit_status,
        files_changed: observation.files_changed,
        git_snapshot: observation.git_snapshot.map(redact_git_snapshot),
    }
}

fn redact_git_snapshot(snapshot: GitSnapshot) -> GitSnapshot {
    GitSnapshot {
        workspace: snapshot.workspace,
        head: snapshot.head,
        clean: snapshot.clean,
        status_short: redact_text(&snapshot.status_short),
        diff: redact_text(&snapshot.diff),
        staged_diff: redact_text(&snapshot.staged_diff),
    }
}

fn event_hash(event: &EvidenceEvent) -> Result<String> {
    let mut event = event.clone();
    event.event_hash.clear();
    let bytes = serde_json::to_vec(&event)
        .with_context(|| format!("serialize event {} for integrity hash", event.id))?;
    let digest = Sha256::digest(bytes);
    Ok(hex_digest(&digest))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
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
