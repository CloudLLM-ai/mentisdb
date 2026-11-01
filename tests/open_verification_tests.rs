//! Open-time verification of governed chains: a tampered file cannot introduce
//! unsigned or forged thoughts into a private segment, a signed checkpoint
//! skips the verified prefix, and truncation is noticed.

use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::AccessMode;
use mentisdb::audience::Audience;
use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
use mentisdb::signing::{thought_payload_v2, to_hex, StoredSignature};
use mentisdb::{MentisDb, StorageAdapterKind, Thought, ThoughtInput, ThoughtType};
use std::io::Write;
use std::path::{Path, PathBuf};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mentisdb_openverify_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[44u8; 32])
}

fn open(dir: &Path) -> MentisDb {
    MentisDb::open_with_key_and_storage_kind(dir, "vault", StorageAdapterKind::Binary).unwrap()
}

fn sign(chain: &MentisDb, text: &str) -> ThoughtInput {
    let head = chain.thoughts().last().unwrap().hash.clone();
    let mut input = ThoughtInput::new(ThoughtType::Insight, text);
    let payload = thought_payload_v2("vault", "builder", &head, &input);
    input.signing_key_id = Some("b1".into());
    input.thought_signature = Some(StoredSignature::encode_v2(&key().sign(&payload).to_bytes()));
    input
}

/// public(0), records and allow (1-2), signed private notes (3, 4).
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
    for text in ["private one", "private two"] {
        let input = sign(&chain, text);
        chain.append_thought("builder", input).unwrap();
    }
    chain.flush().unwrap();
}

// ---- forging helpers: what an attacker with write access to the chain file can do.

fn encode(t: &Thought) -> Vec<u8> {
    let payload = bincode::serde::encode_to_vec(t, bincode::config::standard()).unwrap();
    let mut bytes = (payload.len() as u64).to_le_bytes().to_vec();
    bytes.extend_from_slice(&payload);
    bytes
}

fn chain_path(dir: &Path) -> PathBuf {
    PathBuf::from(open(dir).storage_location())
}

/// Append a correctly chained and correctly hashed thought (`edit` sets the lie).
fn plant(dir: &Path, edit: impl FnOnce(&mut Thought)) {
    let (mut thought, path) = {
        let chain = open(dir);
        (
            chain.thoughts().last().unwrap().clone(),
            PathBuf::from(chain.storage_location()),
        )
    };
    thought.prev_hash = thought.hash.clone();
    thought.index += 1;
    thought.id = uuid::Uuid::new_v4();
    edit(&mut thought);
    mentisdb::seal_thought_hash(&mut thought);
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&encode(&thought)).unwrap();
}

/// Rewrite the whole file, re-chaining and re-hashing every record.
fn rewrite(dir: &Path, edit: impl FnOnce(&mut Vec<Thought>)) {
    let (mut thoughts, path) = {
        let chain = open(dir);
        (
            chain.thoughts().to_vec(),
            PathBuf::from(chain.storage_location()),
        )
    };
    edit(&mut thoughts);
    let mut prev = String::new();
    let mut bytes = Vec::new();
    for (i, t) in thoughts.iter_mut().enumerate() {
        t.index = i as u64;
        t.prev_hash = prev.clone();
        mentisdb::seal_thought_hash(t);
        prev = t.hash.clone();
        bytes.extend_from_slice(&encode(t));
    }
    std::fs::File::create(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}

fn checkpoint(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".verified.json"))
        .expect("checkpoint written")
}

fn contents(chain: &MentisDb, audience: Audience) -> Vec<String> {
    chain
        .visible_thoughts(audience)
        .map(|t| t.content.clone())
        .collect()
}

#[test]
fn a_valid_chain_verifies_clean_and_writes_a_checkpoint() {
    let dir = dir("valid");
    governed(&dir);
    let chain = open(&dir);
    assert!(chain.verification().quarantined.is_empty());
    assert!(!chain.verification().diverged_from_checkpoint);
    assert!(contents(&chain, Audience::Authorized).contains(&"private two".to_string()));
    assert!(chain.verify_signatures().is_clean());
    checkpoint(&dir);
    drop(chain);
    // The second open skips the verified prefix.
    let again = open(&dir);
    assert!(again.verification().checkpoint_used);
    assert_eq!(again.verification().checked, 0);
}

#[test]
fn an_unsigned_thought_planted_in_a_private_segment_is_quarantined() {
    let dir = dir("unsigned");
    governed(&dir);
    let _ = open(&dir); // checkpoint over the honest prefix
    plant(&dir, |t| {
        t.content = "PLANTED unsigned private thought".into();
        t.signing_key_id = None;
        t.thought_signature = None;
        t.agent_id = "mallory".into();
    });
    let chain = open(&dir);
    assert!(
        chain.verification().checkpoint_used,
        "only the new suffix is verified"
    );
    assert_eq!(chain.verification().quarantined, vec![5]);
    assert!(chain.verification().reasons[0].1.contains("unsigned"));
    for audience in [Audience::Authorized, Audience::Public] {
        assert!(!contents(&chain, audience)
            .iter()
            .any(|c| c.contains("PLANTED")));
    }
    assert!(contents(&chain, Audience::Authorized).contains(&"private two".to_string()));
    let report = chain.verify_signatures();
    assert_eq!(report.quarantined, vec![5]);
    assert!(!report.is_clean());
    assert!(chain
        .verify_signatures_for(Audience::Public)
        .quarantined
        .is_empty());
    // The quarantine survives reopening (it is part of the signed checkpoint).
    drop(chain);
    assert_eq!(open(&dir).verification().quarantined, vec![5]);
}

#[test]
fn a_forged_signature_is_quarantined() {
    let dir = dir("forged");
    governed(&dir);
    plant(&dir, |t| {
        // A signature copied from another thought does not cover this content.
        t.content = "FORGED content under a stolen signature".into();
    });
    // The chain was last sealed before the lie; no checkpoint: verify everything.
    let _ = std::fs::remove_file(checkpoint_if_any(&dir));
    let chain = open(&dir);
    assert_eq!(chain.verification().quarantined, vec![5]);
    assert!(!contents(&chain, Audience::Authorized)
        .iter()
        .any(|c| c.contains("FORGED")));
    assert!(contents(&chain, Audience::Authorized).contains(&"private two".to_string()));
}

fn checkpoint_if_any(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".verified.json"))
        .unwrap_or_else(|| dir.join("none"))
}

#[test]
fn a_forged_checkpoint_is_not_trusted() {
    let dir = dir("forged_cp");
    governed(&dir);
    let _ = open(&dir);
    let cp = checkpoint(&dir);
    let original = std::fs::read_to_string(&cp).unwrap();
    plant(&dir, |t| {
        t.content = "PLANTED".into();
        t.signing_key_id = None;
        t.thought_signature = None;
    });
    // Claim the planted thought was already verified: the signature no longer matches.
    let forged = original.replacen("\"count\":5", "\"count\":6", 1).replacen(
        "\"head_hash\":\"",
        "\"head_hash\":\"00",
        1,
    );
    std::fs::write(&cp, forged).unwrap();
    let chain = open(&dir);
    assert!(!chain.verification().checkpoint_used);
    assert_eq!(
        chain.verification().quarantined,
        vec![5],
        "verified from scratch"
    );
}

#[test]
fn a_truncated_chain_is_locked_not_served_as_public() {
    let dir = dir("truncated");
    governed(&dir);
    let _ = open(&dir); // signed checkpoint over 5 thoughts
                        // Cut the file back to the single public thought: the governance records
                        // are gone, so without the checkpoint this chain would look fully public.
    let path = chain_path(&dir);
    let keep = {
        let chain = open(&dir);
        encode(&chain.thoughts()[0]).len()
    };
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..keep]).unwrap();
    let chain = open(&dir);
    assert!(chain.verification().diverged_from_checkpoint);
    assert!(chain.verification().truncated_since_checkpoint);
    for audience in [Audience::Authorized, Audience::Public] {
        assert!(
            contents(&chain, audience).is_empty(),
            "locked chain serves nothing"
        );
    }
    // Re-sealing: the operator inspects the file and deletes the checkpoint.
    drop(chain);
    std::fs::remove_file(checkpoint(&dir)).unwrap();
    let chain = open(&dir);
    assert!(!chain.verification().diverged_from_checkpoint);
    assert_eq!(
        contents(&chain, Audience::Public),
        vec!["public".to_string()]
    );
}

#[test]
fn a_rewritten_prefix_is_locked() {
    let dir = dir("rewritten");
    governed(&dir);
    let _ = open(&dir);
    rewrite(&dir, |thoughts| {
        thoughts[0].content = "rewritten history".into()
    });
    let chain = open(&dir);
    assert!(chain.verification().diverged_from_checkpoint);
    assert!(contents(&chain, Audience::Public).is_empty());
}
