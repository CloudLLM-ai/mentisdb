use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::{
    export_skill, import_skill, migrate_skill_registry, SkillFormat, SkillQuery, SkillRegistry,
    SkillStatus, SkillUpload, SkillVersionContent, MENTISDB_SKILL_REGISTRY_CURRENT_VERSION,
    MENTISDB_SKILL_REGISTRY_V1,
};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_registry_path() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_skill_registry_test_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("mentisdb-skills.bin")
}

#[test]
fn skill_adapters_roundtrip_markdown_and_json() {
    let markdown = r#"---
schema_version: 1
name: mentisdb
description: Durable memory skill
tags: [memory, registry]
triggers: [mentisdb, skill]
warnings: [untrusted-content]
---

# MentisDB

Durable memory skill

## Usage

Use tags and UTC time windows.
"#;

    let document = import_skill(markdown, SkillFormat::Markdown).unwrap();
    assert_eq!(document.name, "mentisdb");
    assert_eq!(document.tags, vec!["memory", "registry"]);
    assert_eq!(document.sections[1].heading, "Usage");

    let json = export_skill(&document, SkillFormat::Json).unwrap();
    let reparsed = import_skill(&json, SkillFormat::Json).unwrap();
    assert_eq!(reparsed, document);

    let rendered_markdown = export_skill(&document, SkillFormat::Markdown).unwrap();
    assert!(rendered_markdown.contains("schema_version: 1"));
    assert!(rendered_markdown.contains("## Usage"));
}

#[test]
fn markdown_import_extracts_frontmatter_and_sections() {
    let markdown = r#"---
schema_version: 1
name: mentisdb
description: Durable memory skill
tags: [memory, registry]
triggers: [mentisdb, skill]
warnings: [untrusted-content]
---

# MentisDB

Durable memory skill

## Usage

Use it well.
"#;

    let document = import_skill(markdown, SkillFormat::Markdown).unwrap();
    assert_eq!(document.name, "mentisdb");
    assert_eq!(document.tags, vec!["memory", "registry"]);
    assert_eq!(document.triggers, vec!["mentisdb", "skill"]);
    assert_eq!(document.sections.len(), 2);
}

#[test]
fn upload_list_search_read_and_status_flows_work() {
    let path = unique_registry_path();
    let mut registry = SkillRegistry::open_at_path(&path).unwrap();
    let markdown = r#"---
schema_version: 1
name: mentisdb
description: Durable memory skill
tags: [memory, registry]
triggers: [mentisdb, skill]
---

# MentisDB

Durable memory skill

## Expert Tricks

Search by UTC day window first.
"#;

    let summary = registry
        .upload_skill(
            SkillUpload::new("astro", SkillFormat::Markdown, markdown)
                .with_agent_identity(Some("Astro"), Some("@gubatron")),
        )
        .unwrap();
    assert_eq!(summary.skill_id, "mentisdb");
    assert_eq!(summary.version_count, 1);
    assert_eq!(summary.latest_uploaded_by_agent_id, "astro");

    let second_summary = registry
        .upload_skill(
            SkillUpload::new(
                "apollo",
                SkillFormat::Json,
                &export_skill(
                    &import_skill(markdown, SkillFormat::Markdown).unwrap(),
                    SkillFormat::Json,
                )
                .unwrap(),
            )
            .with_skill_id("mentisdb")
            .with_agent_identity(Some("Apollo"), Some("@gubatron")),
        )
        .unwrap();
    assert_eq!(second_summary.version_count, 2);
    assert_eq!(second_summary.latest_uploaded_by_agent_id, "apollo");

    let listed = registry.list_skills();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].latest_source_format, SkillFormat::Json);

    let by_tag = registry.search_skills(&SkillQuery {
        tags_any: vec!["memory".to_string()],
        ..SkillQuery::default()
    });
    assert_eq!(by_tag.len(), 1);

    let by_agent = registry.search_skills(&SkillQuery {
        uploaded_by_agent_ids: Some(vec!["astro".to_string()]),
        ..SkillQuery::default()
    });
    assert_eq!(by_agent.len(), 1);

    let by_agent_name = registry.search_skills(&SkillQuery {
        uploaded_by_agent_names: Some(vec!["Apollo".to_string()]),
        ..SkillQuery::default()
    });
    assert_eq!(by_agent_name.len(), 1);

    let by_agent_owner = registry.search_skills(&SkillQuery {
        uploaded_by_agent_owners: Some(vec!["@gubatron".to_string()]),
        ..SkillQuery::default()
    });
    assert_eq!(by_agent_owner.len(), 1);

    let by_text = registry.search_skills(&SkillQuery {
        text: Some("UTC day window".to_string()),
        ..SkillQuery::default()
    });
    assert_eq!(by_text.len(), 1);

    let versions = registry.skill_versions("mentisdb").unwrap();
    assert_eq!(versions.len(), 2);
    let first_version_id = versions[0].version_id;
    assert_ne!(versions[0].version_id, versions[1].version_id);

    let first_markdown = registry
        .read_skill("mentisdb", Some(first_version_id), SkillFormat::Markdown)
        .unwrap();
    assert!(first_markdown.contains("# mentisdb") || first_markdown.contains("# MentisDB"));

    let latest_json = registry
        .read_skill("mentisdb", None, SkillFormat::Json)
        .unwrap();
    assert!(
        latest_json.contains("\"name\": \"mentisdb\"")
            || latest_json.contains("\"name\": \"MentisDB\"")
    );

    let deprecated = registry
        .deprecate_skill("mentisdb", Some("superseded"))
        .unwrap();
    assert_eq!(deprecated.status, SkillStatus::Deprecated);

    let revoked = registry.revoke_skill("mentisdb", Some("unsafe")).unwrap();
    assert_eq!(revoked.status, SkillStatus::Revoked);

    let manifest = registry.manifest();
    assert!(manifest
        .searchable_fields
        .iter()
        .any(|field| field == "uploaded_by_agent_owners"));

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn skill_registry_persists_and_reloads() {
    let path = unique_registry_path();
    {
        let mut registry = SkillRegistry::open_at_path(&path).unwrap();
        registry
            .upload_skill(
                SkillUpload::new(
                    "astro",
                    SkillFormat::Markdown,
                    r#"---
schema_version: 1
name: test-skill
description: A persisted skill
tags: [persisted]
---

# Test Skill

A persisted skill

## Notes

Remember the persisted rule.
"#,
                )
                .with_agent_identity(Some("Astro"), Some("@gubatron")),
            )
            .unwrap();
    }

    let reloaded = SkillRegistry::open_at_path(&path).unwrap();
    let listed = reloaded.list_skills();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].skill_id, "test-skill");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn skill_registry_updates_latest_summary_and_status_indexes_after_mutations() {
    let path = unique_registry_path();
    let mut registry = SkillRegistry::open_at_path(&path).unwrap();

    let alpha = r#"---
schema_version: 1
name: Alpha Skill
description: First version
tags: [alpha]
triggers: [first]
---

# Alpha Skill

First version
"#;
    let beta = r#"---
schema_version: 1
name: Beta Skill
description: Second version
tags: [beta]
triggers: [second]
---

# Beta Skill

Second version
"#;

    registry
        .upload_skill(
            SkillUpload::new("astro", SkillFormat::Markdown, alpha).with_skill_id("tracked-skill"),
        )
        .unwrap();

    assert_eq!(registry.list_skills()[0].name, "Alpha Skill");
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                tags_any: vec!["alpha".to_string()],
                ..SkillQuery::default()
            })
            .len(),
        1
    );
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                names: Some(vec!["Alpha Skill".to_string()]),
                ..SkillQuery::default()
            })
            .len(),
        1
    );

    registry
        .upload_skill(
            SkillUpload::new("astro", SkillFormat::Markdown, beta).with_skill_id("tracked-skill"),
        )
        .unwrap();

    assert_eq!(registry.list_skills()[0].name, "Beta Skill");
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                tags_any: vec!["alpha".to_string()],
                ..SkillQuery::default()
            })
            .len(),
        0
    );
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                tags_any: vec!["beta".to_string()],
                ..SkillQuery::default()
            })
            .len(),
        1
    );
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                names: Some(vec!["Alpha Skill".to_string()]),
                ..SkillQuery::default()
            })
            .len(),
        0
    );
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                names: Some(vec!["Beta Skill".to_string()]),
                ..SkillQuery::default()
            })
            .len(),
        1
    );
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                statuses: Some(vec![SkillStatus::Active]),
                ..SkillQuery::default()
            })
            .len(),
        1
    );

    registry
        .deprecate_skill("tracked-skill", Some("superseded"))
        .unwrap();
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                statuses: Some(vec![SkillStatus::Active]),
                ..SkillQuery::default()
            })
            .len(),
        0
    );
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                statuses: Some(vec![SkillStatus::Deprecated]),
                ..SkillQuery::default()
            })
            .len(),
        1
    );

    registry
        .revoke_skill("tracked-skill", Some("unsafe"))
        .unwrap();
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                statuses: Some(vec![SkillStatus::Deprecated]),
                ..SkillQuery::default()
            })
            .len(),
        0
    );
    assert_eq!(
        registry
            .search_skills(&SkillQuery {
                statuses: Some(vec![SkillStatus::Revoked]),
                ..SkillQuery::default()
            })
            .len(),
        1
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// Helper: a small reusable markdown skill template
// ---------------------------------------------------------------------------

fn skill_markdown(name: &str, description: &str, extra_body: &str) -> String {
    format!(
        r#"---
schema_version: 1
name: {name}
description: {description}
tags: [testing, delta]
triggers: [test]
---

# {name}

{description}

## Details

{extra_body}
"#
    )
}

// ---------------------------------------------------------------------------
// Test 1: skill_first_upload_stores_full_content
// ---------------------------------------------------------------------------

/// Verifies that the first upload of a skill always stores its content in
/// `SkillVersionContent::Full` form with `version_number == 0` and that the
/// recorded `content_hash` is the correct SHA-256 hex digest of the raw text.
#[test]
fn skill_first_upload_stores_full_content() {
    use sha2::{Digest, Sha256};

    let path = unique_registry_path();
    let mut registry = SkillRegistry::open_at_path(&path).unwrap();

    let content = skill_markdown("First Skill", "First upload test", "Initial body text.");
    registry
        .upload_skill(
            SkillUpload::new("astro", SkillFormat::Markdown, &content)
                .with_agent_identity(Some("Astro"), Some("@gubatron")),
        )
        .unwrap();

    let version = registry.skill_version("first-skill", None).unwrap();

    assert_eq!(
        version.version_number, 0,
        "first upload must have version_number 0"
    );

    match &version.content {
        SkillVersionContent::Full { raw } => {
            assert_eq!(
                raw, &content,
                "stored raw content must exactly match what was uploaded"
            );

            // Independently compute the SHA-256 hash and compare.
            let expected_hash = format!("{:x}", Sha256::digest(content.as_bytes()));
            assert_eq!(
                version.content_hash, expected_hash,
                "content_hash must be the SHA-256 hex digest of the uploaded raw text"
            );
        }
        SkillVersionContent::Delta { .. } => {
            panic!("first upload must be stored as Full content, not Delta");
        }
    }

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// Test 2: skill_second_upload_stores_delta
// ---------------------------------------------------------------------------

/// Verifies that v0 is stored as `SkillVersionContent::Full` and v1 is stored
/// as `SkillVersionContent::Delta` with a non-empty patch string, and that
/// `read_skill` reconstructs both versions correctly.
#[test]
fn skill_second_upload_stores_delta() {
    let path = unique_registry_path();
    let mut registry = SkillRegistry::open_at_path(&path).unwrap();

    let content_v0 = skill_markdown(
        "Delta Skill",
        "Version zero content",
        "Original paragraph with some detail.",
    );
    let content_v1 = skill_markdown(
        "Delta Skill",
        "Version zero content",
        "Revised paragraph with updated information and extra line.",
    );

    registry
        .upload_skill(SkillUpload::new(
            "astro",
            SkillFormat::Markdown,
            &content_v0,
        ))
        .unwrap();
    registry
        .upload_skill(
            SkillUpload::new("astro", SkillFormat::Markdown, &content_v1)
                .with_skill_id("delta-skill"),
        )
        .unwrap();

    // Retrieve version summaries to get both version_ids.
    let summaries = registry.skill_versions("delta-skill").unwrap();
    assert_eq!(summaries.len(), 2, "must have exactly two versions");
    assert_eq!(summaries[0].version_number, 0);
    assert_eq!(summaries[1].version_number, 1);

    let v0 = registry
        .skill_version("delta-skill", Some(summaries[0].version_id))
        .unwrap();
    let v1 = registry
        .skill_version("delta-skill", Some(summaries[1].version_id))
        .unwrap();

    // v0 must be Full.
    assert!(
        matches!(&v0.content, SkillVersionContent::Full { .. }),
        "v0 must be stored as Full content"
    );

    // v1 must be Delta with a non-empty patch.
    match &v1.content {
        SkillVersionContent::Delta { patch } => {
            assert!(
                !patch.is_empty(),
                "delta patch between v0 and v1 must be non-empty"
            );
        }
        SkillVersionContent::Full { .. } => {
            panic!("v1 must be stored as Delta content, not Full");
        }
    }

    // Both versions must reconstruct to the originally uploaded text (via read_skill).
    let reconstructed_v0 = registry
        .read_skill(
            "delta-skill",
            Some(summaries[0].version_id),
            SkillFormat::Markdown,
        )
        .unwrap();
    assert!(
        reconstructed_v0.contains("Original paragraph with some detail"),
        "v0 must reconstruct to original content; got: {reconstructed_v0}"
    );

    let reconstructed_v1 = registry
        .read_skill(
            "delta-skill",
            Some(summaries[1].version_id),
            SkillFormat::Markdown,
        )
        .unwrap();
    assert!(
        reconstructed_v1.contains("Revised paragraph with updated information"),
        "v1 must reconstruct to revised content; got: {reconstructed_v1}"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// Test 3: skill_delta_chain_reconstructs_all_versions
// ---------------------------------------------------------------------------

/// Uploads four successive versions of a skill with distinct content changes,
/// then verifies that every version (0 through 3) can be independently
/// reconstructed to match what was originally uploaded.
/// Also confirms that version_numbers are assigned monotonically: 0, 1, 2, 3.
#[test]
fn skill_delta_chain_reconstructs_all_versions() {
    let path = unique_registry_path();
    let mut registry = SkillRegistry::open_at_path(&path).unwrap();

    let versions_content: Vec<String> = (0..4)
        .map(|i| {
            skill_markdown(
                "Chain Skill",
                "Delta chain test",
                &format!("This is revision {i} of the body. Unique token: REVISION_{i}_TOKEN."),
            )
        })
        .collect();

    for content in &versions_content {
        registry
            .upload_skill(
                SkillUpload::new("astro", SkillFormat::Markdown, content)
                    .with_skill_id("chain-skill"),
            )
            .unwrap();
    }

    let summaries = registry.skill_versions("chain-skill").unwrap();
    assert_eq!(
        summaries.len(),
        4,
        "must have four versions after four uploads"
    );

    for (i, summary) in summaries.iter().enumerate() {
        assert_eq!(
            summary.version_number, i as u32,
            "version_number must equal its 0-based index"
        );

        // Reconstruct this version and verify it contains the unique token.
        let reconstructed = registry
            .read_skill(
                "chain-skill",
                Some(summary.version_id),
                SkillFormat::Markdown,
            )
            .unwrap();
        let expected_token = format!("REVISION_{i}_TOKEN");
        assert!(
            reconstructed.contains(&expected_token),
            "version {i} must reconstruct to its original content; token '{expected_token}' not found in: {reconstructed}"
        );
    }

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// Test 4: skill_migration_v1_to_v2
// ---------------------------------------------------------------------------

/// Verifies that `migrate_skill_registry` correctly upgrades a V1 bincode
/// registry file to the current V2 format.
///
/// The test manually constructs and serializes a V1 registry (using mirror
/// structs that match the private V1 shapes byte-for-byte), writes it to
/// disk, calls `migrate_skill_registry`, verifies the migration report, and
/// then opens the migrated registry to confirm the skill is intact.
///
/// Also verifies that calling `migrate_skill_registry` a second time is a
/// no-op (idempotent), returning `Ok(None)`.
#[test]
fn skill_migration_v1_to_v2() {
    use bincode::config::standard as bincode_standard;
    use chrono::Utc;
    use mentisdb::{SkillDocument, SkillSection};
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use uuid::Uuid;

    // --- V1 mirror structs (must match skills.rs private shapes field-for-field) ---

    #[derive(Serialize, Deserialize)]
    struct SkillVersionV1Mirror {
        version_id: Uuid,
        uploaded_at: chrono::DateTime<Utc>,
        uploaded_by_agent_id: String,
        uploaded_by_agent_name: Option<String>,
        uploaded_by_agent_owner: Option<String>,
        source_format: SkillFormat,
        content_hash: String,
        document: SkillDocument,
    }

    #[derive(Serialize, Deserialize)]
    struct SkillEntryV1Mirror {
        skill_id: String,
        created_at: chrono::DateTime<Utc>,
        updated_at: chrono::DateTime<Utc>,
        status: SkillStatus,
        status_reason: Option<String>,
        versions: Vec<SkillVersionV1Mirror>,
    }

    #[derive(Serialize, Deserialize)]
    struct PersistedSkillRegistryV1Mirror {
        version: u32,
        skills: BTreeMap<String, SkillEntryV1Mirror>,
    }

    // --- Build a synthetic V1 registry ---

    let document = SkillDocument {
        schema_version: 1,
        name: "legacy-skill".to_string(),
        description: "A skill from before the delta era.".to_string(),
        tags: vec!["legacy".to_string()],
        triggers: vec!["legacy".to_string()],
        warnings: vec![],
        sections: vec![SkillSection {
            level: 2,
            heading: "Usage".to_string(),
            body: "Use the legacy API.".to_string(),
        }],
    };

    let raw = export_skill(&document, SkillFormat::Markdown).unwrap();
    use sha2::{Digest, Sha256};
    let content_hash = format!("{:x}", Sha256::digest(raw.as_bytes()));

    let version_v1 = SkillVersionV1Mirror {
        version_id: Uuid::new_v4(),
        uploaded_at: Utc::now(),
        uploaded_by_agent_id: "astro".to_string(),
        uploaded_by_agent_name: Some("Astro".to_string()),
        uploaded_by_agent_owner: Some("@gubatron".to_string()),
        source_format: SkillFormat::Markdown,
        content_hash,
        document: document.clone(),
    };

    let entry_v1 = SkillEntryV1Mirror {
        skill_id: "legacy-skill".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        status: SkillStatus::Active,
        status_reason: None,
        versions: vec![version_v1],
    };

    let mut skills = BTreeMap::new();
    skills.insert("legacy-skill".to_string(), entry_v1);

    let v1_registry = PersistedSkillRegistryV1Mirror {
        version: MENTISDB_SKILL_REGISTRY_V1,
        skills,
    };

    let encoded = bincode::serde::encode_to_vec(&v1_registry, bincode_standard())
        .expect("V1 registry must encode to bincode without error");

    // Write the V1 binary to the expected path inside a fresh chain_dir.
    let chain_dir = {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "mentisdb_skill_registry_test_{}_{}",
            std::process::id(),
            n
        ))
    };
    std::fs::create_dir_all(&chain_dir).unwrap();
    let registry_path = chain_dir.join("mentisdb-skills.bin");
    std::fs::write(&registry_path, &encoded).expect("failed to write synthetic V1 registry");

    // --- Run migration ---
    let report = migrate_skill_registry(&chain_dir)
        .expect("migrate_skill_registry must not return an IO error")
        .expect("migrate_skill_registry must return Some(report) for a V1 file");

    assert_eq!(
        report.from_version, MENTISDB_SKILL_REGISTRY_V1,
        "migration must report from_version == 1"
    );
    assert_eq!(
        report.to_version, MENTISDB_SKILL_REGISTRY_CURRENT_VERSION,
        "migration must report to_version == current"
    );
    assert_eq!(
        report.skills_migrated, 1,
        "one skill must have been migrated"
    );
    assert_eq!(
        report.versions_migrated, 1,
        "one version must have been migrated"
    );
    assert_eq!(report.path, registry_path);

    // --- Open migrated registry and verify the skill is intact ---
    let registry =
        SkillRegistry::open_at_path(&registry_path).expect("migrated registry must open cleanly");
    let listed = registry.list_skills();
    assert_eq!(
        listed.len(),
        1,
        "migrated registry must contain exactly one skill"
    );
    assert_eq!(listed[0].skill_id, "legacy-skill");

    let reconstructed = registry
        .read_skill("legacy-skill", None, SkillFormat::Markdown)
        .expect("legacy-skill must be readable after migration");
    assert!(
        reconstructed.contains("Use the legacy API"),
        "migrated skill body must survive round-trip; got: {reconstructed}"
    );

    // Version must have been assigned version_number 0 during migration.
    let summaries = registry.skill_versions("legacy-skill").unwrap();
    assert_eq!(summaries[0].version_number, 0);

    // --- Idempotency: second call returns Ok(None) ---
    let second_run = migrate_skill_registry(&chain_dir)
        .expect("second migrate_skill_registry call must not error");
    assert!(
        second_run.is_none(),
        "migrate_skill_registry must be idempotent: second call returns None"
    );

    let _ = std::fs::remove_dir_all(&chain_dir);
}

// ---------------------------------------------------------------------------
// Test 5: skill_upload_without_signature_succeeds_when_agent_has_no_keys
// ---------------------------------------------------------------------------

/// Explicitly verifies that uploading a skill with `None, None` for
/// `signing_key_id` and `skill_signature` succeeds when the agent has no
/// registered public keys (library-level, no server involved).
#[test]
fn skill_upload_without_signature_succeeds_when_agent_has_no_keys() {
    let path = unique_registry_path();
    let mut registry = SkillRegistry::open_at_path(&path).unwrap();

    let content = skill_markdown("Unsigned Skill", "No signing required", "Plain upload.");

    let result = registry.upload_skill(SkillUpload::new(
        "anonymous-agent",
        SkillFormat::Markdown,
        &content,
    ));

    assert!(
        result.is_ok(),
        "upload with no signature must succeed when agent has no keys; got: {:?}",
        result.err()
    );

    let summary = result.unwrap();
    assert_eq!(summary.skill_id, "unsigned-skill");
    assert_eq!(summary.version_count, 1);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// Test 6: skill_upload_signing_fields_stored_on_version
// ---------------------------------------------------------------------------

/// Verifies that `signing_key_id` and `skill_signature` passed to
/// `upload_skill` are stored verbatim on the resulting `SkillVersion`.
///
/// The library does NOT verify the signature — that is enforced at the server
/// level.  This test confirms the fields are durably persisted so that
/// server-side enforcement can retrieve them on subsequent reads.
#[test]
fn skill_upload_signing_fields_stored_on_version() {
    let path = unique_registry_path();
    let mut registry = SkillRegistry::open_at_path(&path).unwrap();

    let content = skill_markdown(
        "Signed Skill",
        "Signature fields test",
        "Signed upload body.",
    );
    let fake_key_id = "my-key".to_string();
    let fake_sig = vec![0u8; 64];

    registry
        .upload_skill(
            SkillUpload::new("signing-agent", SkillFormat::Markdown, &content)
                .with_signing(Some(fake_key_id.clone()), Some(fake_sig.clone())),
        )
        .unwrap();

    let version = registry.skill_version("signed-skill", None).unwrap();

    assert_eq!(
        version.signing_key_id.as_deref(),
        Some("my-key"),
        "signing_key_id must be stored verbatim on the SkillVersion"
    );
    assert_eq!(
        version.skill_signature.as_deref(),
        Some(fake_sig.as_slice()),
        "skill_signature bytes must be stored verbatim on the SkillVersion"
    );

    // Re-open registry to verify persistence across a reload.
    let reloaded = SkillRegistry::open_at_path(&path).unwrap();
    let reloaded_version = reloaded.skill_version("signed-skill", None).unwrap();
    assert_eq!(
        reloaded_version.signing_key_id.as_deref(),
        Some("my-key"),
        "signing_key_id must survive a registry reload"
    );
    assert_eq!(
        reloaded_version.skill_signature.as_deref(),
        Some(fake_sig.as_slice()),
        "skill_signature must survive a registry reload"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
