use std::{fs, path::Path, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct ActionPolicy {
    rules: Vec<CompiledPolicyRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionPolicyConfig {
    #[serde(default = "default_builtin_rules")]
    pub builtin_rules: bool,
    #[serde(default)]
    pub rules: Vec<ActionPolicyRuleConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionPolicyRuleConfig {
    pub label: String,
    pub pattern: String,
    #[serde(default)]
    pub action: PolicyAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Allow,
    #[default]
    RequireApproval,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyMatch {
    pub label: String,
    pub action: PolicyAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyApprovalGrant {
    pub id: String,
    pub token: String,
    pub session_id: String,
    pub command: String,
    pub rule: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyApprovalRecord {
    pub id: String,
    pub token: String,
    pub session_id: String,
    pub command: String,
    pub rule: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct CompiledPolicyRule {
    label: String,
    regex: Regex,
    action: PolicyAction,
}

impl ActionPolicy {
    pub fn from_config(config: ActionPolicyConfig) -> Result<Self> {
        let mut rules = Vec::new();
        for rule in config.rules {
            rules.push(CompiledPolicyRule {
                label: rule.label,
                regex: Regex::new(&rule.pattern)
                    .with_context(|| format!("compile policy regex {}", rule.pattern))?,
                action: rule.action,
            });
        }
        if config.builtin_rules {
            rules.extend(builtin_rules()?);
        }
        Ok(Self { rules })
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path).with_context(|| format!("read policy {}", path.display()))?;
        let config: ActionPolicyConfig = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse policy {}", path.display()))?;
        Self::from_config(config)
    }

    pub fn evaluate(&self, command: &str) -> Option<PolicyMatch> {
        for rule in &self.rules {
            if rule.regex.is_match(command) {
                if rule.action == PolicyAction::Allow {
                    return None;
                }
                return Some(PolicyMatch {
                    label: rule.label.clone(),
                    action: rule.action,
                });
            }
        }
        None
    }
}

impl Default for ActionPolicy {
    fn default() -> Self {
        Self::from_config(ActionPolicyConfig::default()).expect("built-in policy rules compile")
    }
}

impl Default for ActionPolicyConfig {
    fn default() -> Self {
        Self {
            builtin_rules: true,
            rules: Vec::new(),
        }
    }
}

impl PolicyApprovalRecord {
    pub fn grant(&self) -> PolicyApprovalGrant {
        PolicyApprovalGrant {
            id: self.id.clone(),
            token: self.token.clone(),
            session_id: self.session_id.clone(),
            command: self.command.clone(),
            rule: self.rule.clone(),
            created_at: self.created_at,
            expires_at: self.expires_at,
        }
    }

    pub fn is_valid_for(
        &self,
        session_id: &str,
        command: &str,
        rule: &str,
        token: &str,
        now: DateTime<Utc>,
    ) -> bool {
        self.session_id == session_id
            && self.command == command
            && self.rule == rule
            && self.token == token
            && self.used_at.is_none()
            && self.expires_at > now
    }
}

pub fn approval_expiry(ttl: Duration) -> DateTime<Utc> {
    Utc::now() + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::hours(1))
}

fn default_builtin_rules() -> bool {
    true
}

fn builtin_rules() -> Result<Vec<CompiledPolicyRule>> {
    Ok(vec![
        CompiledPolicyRule {
            regex: Regex::new(
                r"(?i)(?:^|[;&|]\s*)(?:sudo\s+)?rm\s+-(?:[a-z]*r[a-z]*f|[a-z]*f[a-z]*r)\b",
            )?,
            label: "rm -rf".to_string(),
            action: PolicyAction::RequireApproval,
        },
        CompiledPolicyRule {
            regex: Regex::new(r"(?i)\bgit\s+push\b[^\n]*\s--force(?:-with-lease)?\b")?,
            label: "git push --force".to_string(),
            action: PolicyAction::RequireApproval,
        },
        CompiledPolicyRule {
            regex: Regex::new(r"(?i)\bterraform\s+apply\b")?,
            label: "terraform apply".to_string(),
            action: PolicyAction::RequireApproval,
        },
        CompiledPolicyRule {
            regex: Regex::new(r"(?i)\bkubectl\s+delete\b")?,
            label: "kubectl delete".to_string(),
            action: PolicyAction::RequireApproval,
        },
        CompiledPolicyRule {
            regex: Regex::new(r"(?i)\bvault\s+write\b")?,
            label: "vault write".to_string(),
            action: PolicyAction::RequireApproval,
        },
        CompiledPolicyRule {
            regex: Regex::new(r"(?i)\bgh\s+pr\s+merge\b")?,
            label: "gh pr merge".to_string(),
            action: PolicyAction::RequireApproval,
        },
    ])
}
