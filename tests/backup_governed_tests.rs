//! A restore may extend a governed chain with a newer backup only when every
//! record of the suffix is one the policy of its moment would have accepted.

use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::AccessMode;
use mentisdb::backup::{create_backup, restore_backup, BackupOptions, RestoreOptions};
use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
use mentisdb::signing::{thought_payload_v2, to_hex, StoredSignature};
use mentisdb::{AgentRegistry, MentisDb, StorageAdapterKind, ThoughtInput, ThoughtType};
use std::path::{Path, PathBuf};

fn dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("mentisdb_backup_gov_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[21u8; 32])
}

fn open(dir: &Path) -> MentisDb {
    MentisDb::open_with_key_and_storage_kind(dir, "vault", StorageAdapterKind::Binary).unwrap()
}

fn signed(chain: &mut MentisDb, text: &str) {
    let head = chain.thoughts().last().unwrap().hash.clone();
    let mut input = ThoughtInput::new(ThoughtType::Insight, text);
    let payload = thought_payload_v2("vault", "builder", &head, &input);
    input.signing_key_id = Some("b1".into());
    input.thought_signature = Some(StoredSignature::encode_v2(&key().sign(&payload).to_bytes()));
    chain.append_thought("builder", input).unwrap();
}

/// A private chain with one signed thought on disk in `dir`.
fn governed(dir: &Path) {
    let operator = OperatorIdentity::load_or_create(dir).unwrap();
    let mut chain = open(dir);
    chain
        .append("alice", ThoughtType::Insight, "public")
        .unwrap();
    apply(
        &mut chain,
        &operator,
        &OperatorOp::SetMode {
            mode: AccessMode::Allowlist,
            keys: vec![],
        },
    )
    .unwrap();
    apply(
        &mut chain,
        &operator,
        &OperatorOp::AllowKey {
            agent_id: "builder".into(),
            key_id: "b1".into(),
            public_key: to_hex(&key().verifying_key().to_bytes()),
        },
    )
    .unwrap();
    signed(&mut chain, "first private note");
    chain.flush().unwrap();
}

fn copy_files(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() && entry.file_name() != "mentisdb.lock" {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

/// A newer backup of a governed chain fast-forwards it when its suffix verifies.
#[test]
fn restore_fast_forwards_a_governed_chain_with_a_verified_suffix() {
    let local = dir("local");
    governed(&local);
    let remote = dir("remote");
    copy_files(&local, &remote);
    {
        let mut chain = open(&remote);
        signed(&mut chain, "second private note");
        chain.flush().unwrap();
    }
    let archive = remote.join("remote.mentis");
    create_backup(&BackupOptions {
        source_dir: remote.clone(),
        output_path: Some(archive.clone()),
        flush_before_backup: false,
        include_tls: false,
    })
    .unwrap();
    restore_backup(archive, local.clone(), RestoreOptions { overwrite: false }).unwrap();
    let chain = open(&local);
    let contents: Vec<&str> = chain
        .thoughts()
        .iter()
        .map(|t| t.content.as_str())
        .collect();
    assert!(contents.contains(&"second private note"), "{contents:?}");
    // It was extended in place, not kept as a renamed copy.
    let registry = mentisdb::load_registered_chains(&local).unwrap();
    assert_eq!(registry.chains.keys().collect::<Vec<_>>(), vec!["vault"]);
    assert!(chain.verify_signatures().is_clean());
}

/// The suffix check refuses what the policy of its moment would not have accepted.
#[test]
fn suffix_check_refuses_signatures_it_cannot_verify() {
    let dir = dir("suffix");
    governed(&dir);
    let chain = open(&dir);
    let start = chain
        .thoughts()
        .iter()
        .position(|t| t.thought_type == ThoughtType::Insight && t.agent_id == "builder")
        .unwrap();
    // With the chain's own registry the signed suffix checks out...
    assert!(chain
        .verify_governed_suffix(start, chain.agent_registry())
        .is_ok());
    // ...with no registry (the key is unknown) it does not, and neither does a
    // suffix that starts earlier and so includes policy records signed by a key
    // the empty registry cannot vouch for.
    assert!(chain
        .verify_governed_suffix(start, &AgentRegistry::default())
        .is_err());
    assert!(chain
        .verify_governed_suffix(1, &AgentRegistry::default())
        .is_err());
}

/// A suffix that cannot be verified (the backup has lost the registry holding
/// the signer's key) is not spliced in: the backup is kept as a separate chain
/// and the local chain is untouched.
#[test]
fn restore_keeps_an_unverifiable_suffix_as_a_separate_chain() {
    let local = dir("unverifiable_local");
    governed(&local);
    let before = open(&local).thoughts().len();
    let remote = dir("unverifiable_remote");
    copy_files(&local, &remote);
    {
        let mut chain = open(&remote);
        signed(&mut chain, "second private note");
        chain.flush().unwrap();
    }
    for entry in std::fs::read_dir(&remote).unwrap() {
        let path = entry.unwrap().path();
        if path.to_string_lossy().ends_with(".agents.json") {
            std::fs::remove_file(path).unwrap();
        }
    }
    let archive = remote.join("remote.mentis");
    create_backup(&BackupOptions {
        source_dir: remote.clone(),
        output_path: Some(archive.clone()),
        flush_before_backup: false,
        include_tls: false,
    })
    .unwrap();
    restore_backup(archive, local.clone(), RestoreOptions { overwrite: false }).unwrap();
    assert_eq!(
        open(&local).thoughts().len(),
        before,
        "local chain untouched"
    );
    let registry = mentisdb::load_registered_chains(&local).unwrap();
    assert_eq!(
        registry.chains.len(),
        2,
        "kept as a separate chain: {:?}",
        registry.chains.keys()
    );
}
