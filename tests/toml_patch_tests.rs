#![allow(dead_code)]

mod integrations {
    pub use mentisdb::integrations::IntegrationFileFormat;
}

#[path = "../src/integrations/files.rs"]
mod files_impl;

use files_impl::{render_managed_file, ManagedFile, TomlPatch, TomlValue};
use std::io;

#[test]
fn toml_if_missing_rejects_mid_path_non_table_collisions() {
    let file = ManagedFile::toml(
        "/tmp/config.toml",
        TomlPatch::new().ensure_path(
            ["mcp_servers", "mentisdb", "url"],
            TomlValue::from("http://127.0.0.1:9471"),
        ),
    );

    let error = render_managed_file(Some("mcp_servers = \"oops\"\n"), &file).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains("collides with a non-table value"));
}
