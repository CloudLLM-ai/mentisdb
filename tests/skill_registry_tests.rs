use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::{export_skill, import_skill, SkillFormat, SkillQuery, SkillRegistry, SkillStatus};

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
            None,
            "astro",
            Some("Astro"),
            Some("@gubatron"),
            SkillFormat::Markdown,
            markdown,
        )
        .unwrap();
    assert_eq!(summary.skill_id, "mentisdb");
    assert_eq!(summary.version_count, 1);
    assert_eq!(summary.latest_uploaded_by_agent_id, "astro");

    let second_summary = registry
        .upload_skill(
            Some("mentisdb"),
            "apollo",
            Some("Apollo"),
            Some("@gubatron"),
            SkillFormat::Json,
            &export_skill(
                &import_skill(markdown, SkillFormat::Markdown).unwrap(),
                SkillFormat::Json,
            )
            .unwrap(),
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
                None,
                "astro",
                Some("Astro"),
                Some("@gubatron"),
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
            .unwrap();
    }

    let reloaded = SkillRegistry::open_at_path(&path).unwrap();
    let listed = reloaded.list_skills();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].skill_id, "test-skill");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
