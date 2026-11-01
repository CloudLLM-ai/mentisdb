//! Operator authority: the library operations behind the dashboard and the CLI,
//! the operator key file, and the self-lockout guards.

use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{policy_thought_input, AccessMode, AllowedKey, PolicyRecord};
use mentisdb::audience::Audience;
use mentisdb::operator::{apply, KeySpec, OperatorIdentity, OperatorOp, OPERATOR_AGENT_ID};
use mentisdb::signing::{thought_payload_v2, to_hex, StoredSignature};
use mentisdb::{MentisDb, StorageAdapterKind, ThoughtInput, ThoughtType};

fn dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mentisdb_operator_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn open(dir: &std::path::Path) -> MentisDb {
    MentisDb::open_with_key_and_storage_kind(dir, "vault", StorageAdapterKind::Binary).unwrap()
}

fn agent_key(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let hex = to_hex(&key.verifying_key().to_bytes());
    (key, hex)
}

fn set_mode(mode: AccessMode) -> OperatorOp {
    OperatorOp::SetMode { mode, keys: vec![] }
}

fn signed_append(chain: &mut MentisDb, agent: &str, key_id: &str, key: &SigningKey, text: &str) {
    let head = chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default();
    let mut input = ThoughtInput::new(ThoughtType::Insight, text);
    let payload = thought_payload_v2(chain.chain_key(), agent, &head, &input);
    input.signing_key_id = Some(key_id.to_string());
    input.thought_signature = Some(StoredSignature::encode_v2(&key.sign(&payload).to_bytes()));
    chain.append_thought(agent, input).unwrap();
}

#[test]
fn operator_key_is_created_once_with_owner_only_permissions() {
    let dir = dir("identity");
    let first = OperatorIdentity::load_or_create(&dir).unwrap();
    let again = OperatorIdentity::load_or_create(&dir).unwrap();
    assert_eq!(first.public_key(), again.public_key());
    let path = OperatorIdentity::key_path(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    // A damaged key file is an error, never silently replaced.
    std::fs::write(&path, "not a key").unwrap();
    assert!(OperatorIdentity::load_or_create(&dir).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a key");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn going_private_makes_writes_need_a_signature_and_hides_the_record() {
    let dir = dir("private");
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain = open(&dir);
    chain
        .append("alice", ThoughtType::Insight, "public note")
        .unwrap();
    let shown = apply(&mut chain, &operator, &set_mode(AccessMode::Allowlist)).unwrap();
    assert_eq!(shown["changed"], true);
    assert_eq!(chain.access_policy().mode, AccessMode::Allowlist);
    assert!(chain
        .append("alice", ThoughtType::Insight, "unsigned")
        .is_err());
    // The operator is the first governor and its record is private.
    assert!(chain.access_policy().contains(
        OPERATOR_AGENT_ID,
        &operator.key_id(),
        &operator.public_key()
    ));
    let record = chain.thoughts().last().unwrap();
    assert_eq!(record.thought_type, ThoughtType::AccessPolicy);
    assert!(!chain.is_visible(record, Audience::Public));
    assert!(chain.is_visible(&chain.thoughts()[0], Audience::Public));
    // The same call again is a no-op.
    let again = apply(&mut chain, &operator, &set_mode(AccessMode::Allowlist)).unwrap();
    assert_eq!(again["changed"], false);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allowing_a_key_registers_it_and_lets_it_write_and_read() {
    let dir = dir("allow");
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain = open(&dir);
    // Not allowed before the chain is private.
    let (key, hex) = agent_key(4);
    let allow = OperatorOp::AllowKey {
        agent_id: "builder".into(),
        key_id: "builder-1".into(),
        public_key: hex.clone(),
    };
    assert!(apply(&mut chain, &operator, &allow).is_err());
    apply(&mut chain, &operator, &set_mode(AccessMode::Allowlist)).unwrap();
    apply(&mut chain, &operator, &allow).unwrap();
    assert!(chain.is_read_authorized("builder", "builder-1", &key.verifying_key().to_bytes()));
    signed_append(&mut chain, "builder", "builder-1", &key, "from the builder");
    // An unlisted key that is merely registered neither writes nor reads.
    let (stranger, stranger_hex) = agent_key(5);
    apply(
        &mut chain,
        &operator,
        &OperatorOp::RegisterKey {
            agent_id: "stranger".into(),
            key_id: "s-1".into(),
            public_key: stranger_hex,
        },
    )
    .unwrap();
    assert!(!chain.is_read_authorized("stranger", "s-1", &stranger.verifying_key().to_bytes()));
    let head = chain.thoughts().last().unwrap().hash.clone();
    let mut input = ThoughtInput::new(ThoughtType::Insight, "let me in");
    let payload = thought_payload_v2("vault", "stranger", &head, &input);
    input.signing_key_id = Some("s-1".into());
    input.thought_signature = Some(StoredSignature::encode_v2(
        &stranger.sign(&payload).to_bytes(),
    ));
    assert!(chain.append_thought("stranger", input).is_err());
    // Allow is idempotent.
    let again = apply(&mut chain, &operator, &allow).unwrap();
    assert_eq!(again["changed"], false);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn denying_a_key_stops_it_at_once_but_never_removes_the_operator() {
    let dir = dir("deny");
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain = open(&dir);
    let (key, hex) = agent_key(6);
    apply(&mut chain, &operator, &set_mode(AccessMode::Allowlist)).unwrap();
    let allow = OperatorOp::AllowKey {
        agent_id: "builder".into(),
        key_id: "b1".into(),
        public_key: hex,
    };
    apply(&mut chain, &operator, &allow).unwrap();
    signed_append(&mut chain, "builder", "b1", &key, "ok");
    let deny = OperatorOp::DenyKey {
        agent_id: "builder".into(),
        key_id: "b1".into(),
    };
    apply(&mut chain, &operator, &deny).unwrap();
    assert!(!chain.is_read_authorized("builder", "b1", &key.verifying_key().to_bytes()));
    // Denying it again, or a key that was never allowed, is an error.
    assert!(apply(&mut chain, &operator, &deny).is_err());
    // The operator key is the recovery path and cannot be removed.
    let remove_operator = OperatorOp::DenyKey {
        agent_id: OPERATOR_AGENT_ID.into(),
        key_id: operator.key_id(),
    };
    let error = apply(&mut chain, &operator, &remove_operator).unwrap_err();
    assert!(error.to_string().contains("recovery"), "{error}");
    assert!(apply(
        &mut chain,
        &operator,
        &OperatorOp::RevokeKey {
            agent_id: OPERATOR_AGENT_ID.into(),
            key_id: operator.key_id()
        }
    )
    .is_err());
    assert!(apply(
        &mut chain,
        &operator,
        &OperatorOp::DisableAgent {
            agent_id: OPERATOR_AGENT_ID.into()
        }
    )
    .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: a policy record could drop every governor, after which nobody
/// (not even the operator) could ever change the policy again.
#[test]
fn a_record_that_leaves_no_governor_is_refused() {
    let dir = dir("lockout");
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain = open(&dir);
    apply(&mut chain, &operator, &set_mode(AccessMode::Allowlist)).unwrap();
    // Build an `open` record with an empty key set by hand and sign it as the
    // operator: the library refuses it.
    let seq = chain.access_policy().seq.unwrap() + 1;
    let prev = chain.access_policy().head_record_hash.clone().unwrap();
    let record = PolicyRecord::new(seq, AccessMode::Open, vec![], &prev).unwrap();
    let head = chain.thoughts().last().unwrap().hash.clone();
    let payload = thought_payload_v2(
        "vault",
        OPERATOR_AGENT_ID,
        &head,
        &policy_thought_input(&record),
    );
    let seed_text = std::fs::read_to_string(OperatorIdentity::key_path(&dir)).unwrap();
    let seed: [u8; 32] = mentisdb::signing::from_hex(seed_text.trim())
        .unwrap()
        .try_into()
        .unwrap();
    let signature =
        StoredSignature::encode_v2(&SigningKey::from_bytes(&seed).sign(&payload).to_bytes());
    let error = chain
        .set_access_policy(OPERATOR_AGENT_ID, &record, &operator.key_id(), signature)
        .unwrap_err();
    assert!(
        error.to_string().contains("at least one governor"),
        "{error}"
    );
    // The chain still has a working policy.
    assert_eq!(chain.access_policy().mode, AccessMode::Allowlist);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn going_public_closes_the_private_segment_and_keeps_it_private() {
    let dir = dir("public");
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain = open(&dir);
    let (key, hex) = agent_key(7);
    chain
        .append("alice", ThoughtType::Insight, "before")
        .unwrap();
    apply(&mut chain, &operator, &set_mode(AccessMode::Allowlist)).unwrap();
    apply(
        &mut chain,
        &operator,
        &OperatorOp::AllowKey {
            agent_id: "builder".into(),
            key_id: "b1".into(),
            public_key: hex,
        },
    )
    .unwrap();
    signed_append(&mut chain, "builder", "b1", &key, "private note");
    apply(&mut chain, &operator, &set_mode(AccessMode::Open)).unwrap();
    chain
        .append("alice", ThoughtType::Insight, "public again")
        .unwrap();
    let public: Vec<&str> = chain
        .visible_thoughts(Audience::Public)
        .map(|t| t.content.as_str())
        .collect();
    assert_eq!(public, vec!["before", "public again"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn set_mode_can_allow_extra_keys_in_one_step() {
    let dir = dir("extra");
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain = open(&dir);
    let (key, hex) = agent_key(8);
    let spec: KeySpec = format!("builder:b1:{hex}").parse().unwrap();
    apply(
        &mut chain,
        &operator,
        &OperatorOp::SetMode {
            mode: AccessMode::Allowlist,
            keys: vec![spec],
        },
    )
    .unwrap();
    assert!(chain.is_read_authorized("builder", "b1", &key.verifying_key().to_bytes()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn registering_keys_validates_them() {
    let dir = dir("register");
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain = open(&dir);
    let register = |public_key: String| OperatorOp::RegisterKey {
        agent_id: "a".into(),
        key_id: "k".into(),
        public_key,
    };
    // Weak (identity) point, wrong length, not hex.
    let mut identity = [0u8; 32];
    identity[0] = 1;
    for bad in [to_hex(&identity), "ab".repeat(16), "zz".repeat(32)] {
        assert!(apply(&mut chain, &operator, &register(bad)).is_err());
    }
    let (_, hex) = agent_key(9);
    apply(&mut chain, &operator, &register(hex.clone())).unwrap();
    // The same id cannot be rebound to another key.
    let (_, other) = agent_key(10);
    assert!(apply(&mut chain, &operator, &register(other)).is_err());
    // Revoked ids stay burned.
    apply(
        &mut chain,
        &operator,
        &OperatorOp::RevokeKey {
            agent_id: "a".into(),
            key_id: "k".into(),
        },
    )
    .unwrap();
    assert!(apply(&mut chain, &operator, &register(hex)).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn disabling_an_agent_withdraws_its_authority() {
    let dir = dir("disable");
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain = open(&dir);
    let (key, hex) = agent_key(11);
    apply(&mut chain, &operator, &set_mode(AccessMode::Allowlist)).unwrap();
    apply(
        &mut chain,
        &operator,
        &OperatorOp::AllowKey {
            agent_id: "builder".into(),
            key_id: "b1".into(),
            public_key: hex,
        },
    )
    .unwrap();
    let pk = key.verifying_key().to_bytes();
    assert!(chain.is_read_authorized("builder", "b1", &pk));
    apply(
        &mut chain,
        &operator,
        &OperatorOp::DisableAgent {
            agent_id: "builder".into(),
        },
    )
    .unwrap();
    assert!(!chain.is_read_authorized("builder", "b1", &pk));
    apply(
        &mut chain,
        &operator,
        &OperatorOp::EnableAgent {
            agent_id: "builder".into(),
        },
    )
    .unwrap();
    assert!(chain.is_read_authorized("builder", "b1", &pk));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn operator_commands_parse_strictly() {
    for good in [
        r#"{"op":"show"}"#,
        r#"{"op":"set_mode","mode":"private"}"#,
        r#"{"op":"set_mode","mode":"public","keys":["a:k:0000000000000000000000000000000000000000000000000000000000000000"]}"#,
        r#"{"op":"revoke_key","agent_id":"a","key_id":"k"}"#,
    ] {
        serde_json::from_str::<OperatorOp>(good).unwrap_or_else(|e| panic!("{good}: {e}"));
    }
    for bad in [
        r#"{"op":"sudo"}"#,
        r#"{"op":"set_mode","mode":"banana"}"#,
        r#"{"op":"allow_key","agent_id":"a"}"#,
        r#"{"op":"set_mode","mode":"open","keys":["not-a-spec"]}"#,
        r#"[]"#,
    ] {
        assert!(serde_json::from_str::<OperatorOp>(bad).is_err(), "{bad}");
    }
}

/// Governance records signed by an agent's own governor key (the way chains were
/// governed before the operator model) keep working next to operator records.
#[test]
fn legacy_governor_signed_records_still_apply() {
    let dir = dir("legacy");
    let mut chain = open(&dir);
    let (key, _) = agent_key(12);
    chain
        .upsert_agent("gov", Some("gov"), None, None, None)
        .unwrap();
    let public = key.verifying_key().to_bytes().to_vec();
    let now = chrono::Utc::now().timestamp();
    let payload = mentisdb::signing::key_registration_payload("vault", "gov", "g1", &public, now);
    let proof = mentisdb::access::KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: key.sign(&payload).to_bytes().to_vec(),
    };
    chain
        .add_agent_key(
            "gov",
            "g1",
            mentisdb::PublicKeyAlgorithm::Ed25519,
            public.clone(),
            &proof,
        )
        .unwrap();
    let record = PolicyRecord::new(
        0,
        AccessMode::Allowlist,
        vec![AllowedKey::new("gov", "g1", &public)],
        "",
    )
    .unwrap();
    let head = chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default();
    let payload = thought_payload_v2("vault", "gov", &head, &policy_thought_input(&record));
    chain
        .set_access_policy(
            "gov",
            &record,
            "g1",
            StoredSignature::encode_v2(&key.sign(&payload).to_bytes()),
        )
        .unwrap();
    assert!(chain.is_read_authorized("gov", "g1", &key.verifying_key().to_bytes()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn operator_commands_bound_their_inputs() {
    let dir = dir("bounds");
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain = open(&dir);
    apply(&mut chain, &operator, &set_mode(AccessMode::Allowlist)).unwrap();
    let (_, hex) = agent_key(14);
    for bad_id in ["", "   ", &"a".repeat(257), "line\nbreak", "tab\there"] {
        let allow = OperatorOp::AllowKey {
            agent_id: bad_id.into(),
            key_id: "k".into(),
            public_key: hex.clone(),
        };
        assert!(apply(&mut chain, &operator, &allow).is_err(), "{bad_id:?}");
        let key = OperatorOp::RevokeKey {
            agent_id: "a".into(),
            key_id: bad_id.into(),
        };
        assert!(apply(&mut chain, &operator, &key).is_err(), "{bad_id:?}");
    }
    // Too many keys at once.
    let many: Vec<KeySpec> = (0..1100)
        .map(|i| KeySpec {
            agent_id: format!("a{i}"),
            key_id: "k".into(),
            public_key: hex.clone(),
        })
        .collect();
    assert!(apply(
        &mut chain,
        &operator,
        &OperatorOp::SetMode {
            mode: AccessMode::Allowlist,
            keys: many
        }
    )
    .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
