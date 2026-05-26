use std::{path::PathBuf, time::Duration};

use agent_pty::{
    daemon::{Request, ResponsePayload, handle_request},
    evidence::{Action, EventKind},
    policy::{ActionPolicy, ActionPolicyConfig, ActionPolicyRuleConfig, PolicyAction},
    session::{SessionBackend, SessionManager, WaitCondition},
};
use tempfile::TempDir;

#[test]
fn approval_tokens_are_one_time_evidence_backed_gates() {
    let temp = TempDir::new().unwrap();
    let manager = SessionManager::new(temp.path().join("logs")).unwrap();
    let command = "printf 'approved-policy\\n' # vault write";

    handle_request(
        &manager,
        Request::New {
            id: "approval-flow".to_string(),
            repo: temp.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            rows: 24,
            cols: 80,
            env: Default::default(),
            backend: SessionBackend::Pty,
        },
    )
    .unwrap();

    let denied = handle_request(
        &manager,
        Request::Send {
            id: "approval-flow".to_string(),
            text: command.to_string(),
            enter: true,
            approval: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(denied.contains("vault write requires approval"));

    let ResponsePayload::PolicyApproval(approval) = handle_request(
        &manager,
        Request::Approve {
            id: "approval-flow".to_string(),
            command: command.to_string(),
            rule: Some("vault write".to_string()),
            ttl_ms: 60_000,
        },
    )
    .unwrap() else {
        panic!("expected approval grant");
    };

    handle_request(
        &manager,
        Request::Send {
            id: "approval-flow".to_string(),
            text: command.to_string(),
            enter: true,
            approval: Some(approval.token.clone()),
        },
    )
    .unwrap();
    manager
        .wait(
            "approval-flow",
            WaitCondition::Regex {
                pattern: "approved-policy".to_string(),
            },
            Duration::from_secs(3),
        )
        .unwrap();

    let reused = handle_request(
        &manager,
        Request::Send {
            id: "approval-flow".to_string(),
            text: command.to_string(),
            enter: true,
            approval: Some(approval.token),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(reused.contains("vault write requires approval"));

    let events = manager.replay("approval-flow").unwrap();
    assert!(events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::Action(Action::PolicyApprovalCreated { rule, approval_id, .. })
                if rule == "vault write" && approval_id == &approval.id
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::Action(Action::PolicyApproved { rule, approval_id, .. })
                if rule == "vault write" && approval_id == &approval.id
        )
    }));

    manager.kill("approval-flow").unwrap();
}

#[test]
fn json_policy_can_deny_custom_commands_and_allow_scoped_exceptions() {
    let temp = TempDir::new().unwrap();
    let policy = ActionPolicy::from_config(ActionPolicyConfig {
        builtin_rules: true,
        rules: vec![
            ActionPolicyRuleConfig {
                label: "custom deny".to_string(),
                pattern: "blocked-policy-command".to_string(),
                action: PolicyAction::Deny,
            },
            ActionPolicyRuleConfig {
                label: "allow tmp cleanup".to_string(),
                pattern: r"rm -rf /tmp/agent-pty-policy-allowed".to_string(),
                action: PolicyAction::Allow,
            },
        ],
    })
    .unwrap();
    let manager = SessionManager::new_with_policy(temp.path().join("logs"), policy).unwrap();

    handle_request(
        &manager,
        Request::New {
            id: "configured-policy".to_string(),
            repo: temp.path().to_path_buf(),
            shell: PathBuf::from("/bin/sh"),
            rows: 24,
            cols: 80,
            env: Default::default(),
            backend: SessionBackend::Pty,
        },
    )
    .unwrap();

    let denied = handle_request(
        &manager,
        Request::Send {
            id: "configured-policy".to_string(),
            text: "printf 'blocked-policy-command\\n'".to_string(),
            enter: true,
            approval: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(denied.contains("custom deny is denied by policy"));

    handle_request(
        &manager,
        Request::Send {
            id: "configured-policy".to_string(),
            text: "printf 'allowed-policy\\n' # rm -rf /tmp/agent-pty-policy-allowed".to_string(),
            enter: true,
            approval: None,
        },
    )
    .unwrap();
    manager
        .wait(
            "configured-policy",
            WaitCondition::Regex {
                pattern: "allowed-policy".to_string(),
            },
            Duration::from_secs(3),
        )
        .unwrap();

    manager.kill("configured-policy").unwrap();
}
