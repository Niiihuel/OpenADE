use super::*;

fn account() -> Credential {
    Credential {
        version: 1,
        provider_id: "codex".into(),
        access_token: "fixture-access-token".into(),
        refresh_token: Some("fixture-refresh-token".into()),
        client_id: Some("fixture-client".into()),
        subject: Some("fixture-subject".into()),
        expires_at: Some(now() + 3600),
        scopes: vec!["chatgpt.tokens.use.direct".into()],
        oauth: true,
        ..Default::default()
    }
}

#[test]
fn both_clients_observe_token_rotation_and_revocation() {
    let root = tempfile::tempdir().unwrap();
    let mut record = account();
    save(root.path(), &record).unwrap();
    let desktop = codex_token_source(root.path().into()).unwrap();
    let remote = codex_token_source(root.path().into()).unwrap();
    assert_eq!(desktop().unwrap(), "fixture-access-token");
    record.access_token = "rotated-fixture-token".into();
    save(root.path(), &record).unwrap();
    assert_eq!(remote().unwrap(), "rotated-fixture-token");
    assert_eq!(desktop().unwrap(), "rotated-fixture-token");
    record.disabled = true;
    save(root.path(), &record).unwrap();
    assert!(desktop().is_err());
    assert!(remote().is_err());
}

#[test]
fn a_live_source_cannot_silently_switch_account_or_registration() {
    for changed in ["subject", "client"] {
        let root = tempfile::tempdir().unwrap();
        let mut record = account();
        save(root.path(), &record).unwrap();
        let source = codex_token_source(root.path().into()).unwrap();
        if changed == "subject" {
            record.subject = Some("other-account".into());
        } else {
            record.client_id = Some("other-registration".into());
        }
        save(root.path(), &record).unwrap();
        assert!(source().is_err());
    }
}

#[test]
fn plan_scope_and_expiration_are_enforced_without_network() {
    let root = tempfile::tempdir().unwrap();
    let mut record = account();
    record.scopes.clear();
    save(root.path(), &record).unwrap();
    assert!(read_key(root.path(), "OPENIDE_CODEX_ACCESS_TOKEN").is_none());
    record.scopes = vec!["chatgpt.tokens.use.direct".into()];
    record.expires_at = Some(now());
    save(root.path(), &record).unwrap();
    assert!(read_key(root.path(), "OPENIDE_CODEX_ACCESS_TOKEN").is_none());
}

#[test]
fn refresh_response_preserves_registration_and_requires_scope() {
    let mut record = account();
    update_tokens(
        &mut record,
        &serde_json::json!({
            "token_type":"Bearer", "access_token":"new-fixture-token",
            "refresh_token":"new-refresh-token", "expires_in":3600,
            "scope":"openid chatgpt.tokens.use.direct"
        }),
    )
    .unwrap();
    assert_eq!(record.client_id.as_deref(), Some("fixture-client"));
    assert_eq!(record.subject.as_deref(), Some("fixture-subject"));
    assert_eq!(record.refresh_token.as_deref(), Some("new-refresh-token"));
    let error = update_tokens(
        &mut record,
        &serde_json::json!({
            "token_type":"Bearer", "access_token":"do-not-echo-this-token",
            "expires_in":3600, "scope":"openid"
        }),
    )
    .unwrap_err();
    assert!(!error.contains("do-not-echo-this-token"));
}

#[test]
fn unsafe_provider_names_and_oversize_records_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    assert!(load(root.path(), "../codex").is_err());
    assert!(lock(root.path(), "../codex", &AtomicBool::new(false)).is_err());
    let mut record = account();
    record.provider_id = "../codex".into();
    assert!(save(root.path(), &record).is_err());
    record.provider_id = "codex".into();
    record.access_token = "x".repeat(MAX_RECORD as usize);
    assert!(save(root.path(), &record).is_err());
}

#[cfg(unix)]
#[test]
fn credential_files_are_private_and_symlinks_are_rejected() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    save(root.path(), &account()).unwrap();
    let dir = root.path().join("provider-credentials");
    let path = dir.join("codex.json");
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load(root.path(), "codex").is_err());
    fs::remove_file(&path).unwrap();
    let outside = root.path().join("outside.json");
    fs::write(&outside, b"{}").unwrap();
    symlink(&outside, &path).unwrap();
    assert!(load(root.path(), "codex").is_err());
}

#[test]
fn refresh_lock_can_be_cancelled_without_blocking() {
    let root = tempfile::tempdir().unwrap();
    let _held = lock(root.path(), "codex", &AtomicBool::new(false)).unwrap();
    assert!(lock(root.path(), "codex", &AtomicBool::new(true)).is_err());
}
