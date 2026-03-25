//! Shared file mutation helpers for integration setup.

use crate::integrations::IntegrationFileFormat;
use serde_json::{Map, Value};
use std::io;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value as TomlEditValue};

/// Whether a patch overwrites an existing value or only fills missing state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatchMode {
    /// Always replace the value at the requested path.
    Replace,
    /// Only set the value when the requested path is currently absent.
    IfMissing,
}

/// One managed integration file plus the patch strategy used to render it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ManagedFile {
    path: PathBuf,
    format: IntegrationFileFormat,
    patch: ManagedFilePatch,
}

impl ManagedFile {
    pub(crate) fn json(path: impl Into<PathBuf>, patch: JsonPatch) -> Self {
        Self {
            path: path.into(),
            format: IntegrationFileFormat::Json,
            patch: ManagedFilePatch::Json(patch),
        }
    }

    pub(crate) fn toml(path: impl Into<PathBuf>, patch: TomlPatch) -> Self {
        Self {
            path: path.into(),
            format: IntegrationFileFormat::Toml,
            patch: ManagedFilePatch::Toml(patch),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn markdown(path: impl Into<PathBuf>, patch: ManagedTextPatch) -> Self {
        Self {
            path: path.into(),
            format: IntegrationFileFormat::Markdown,
            patch: ManagedFilePatch::Text(patch),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[allow(dead_code)]
    pub(crate) fn format(&self) -> IntegrationFileFormat {
        self.format
    }
}

#[derive(Debug, Clone, PartialEq)]
enum ManagedFilePatch {
    Json(JsonPatch),
    Toml(TomlPatch),
    Text(ManagedTextPatch),
}

#[derive(Debug, Clone, PartialEq)]
struct JsonPatchOp {
    path: Vec<String>,
    value: Value,
    mode: PatchMode,
}

/// Structured JSON patch helper for idempotent nested config writes.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct JsonPatch {
    ops: Vec<JsonPatchOp>,
}

impl JsonPatch {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_path<I, S>(mut self, path: I, value: Value) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ops.push(JsonPatchOp {
            path: path.into_iter().map(Into::into).collect(),
            value,
            mode: PatchMode::Replace,
        });
        self
    }

    pub(crate) fn ensure_path<I, S>(mut self, path: I, value: Value) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ops.push(JsonPatchOp {
            path: path.into_iter().map(Into::into).collect(),
            value,
            mode: PatchMode::IfMissing,
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TomlPatchOp {
    path: Vec<String>,
    value: TomlValue,
    mode: PatchMode,
}

/// Minimal TOML value model used by integration writers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TomlValue {
    /// A TOML string literal.
    String(String),
    /// A TOML boolean.
    Bool(bool),
    /// A TOML integer.
    Integer(i64),
}

impl TomlValue {
    fn to_toml_edit(&self) -> TomlEditValue {
        match self {
            Self::String(value) => TomlEditValue::from(value.clone()),
            Self::Bool(value) => TomlEditValue::from(*value),
            Self::Integer(value) => TomlEditValue::from(*value),
        }
    }
}

impl From<&str> for TomlValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for TomlValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<bool> for TomlValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for TomlValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

/// Structured TOML patch helper for idempotent nested config writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TomlPatch {
    ops: Vec<TomlPatchOp>,
}

impl TomlPatch {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_path<I, S>(mut self, path: I, value: TomlValue) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ops.push(TomlPatchOp {
            path: path.into_iter().map(Into::into).collect(),
            value,
            mode: PatchMode::Replace,
        });
        self
    }

    #[allow(dead_code)]
    pub(crate) fn ensure_path<I, S>(mut self, path: I, value: TomlValue) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ops.push(TomlPatchOp {
            path: path.into_iter().map(Into::into).collect(),
            value,
            mode: PatchMode::IfMissing,
        });
        self
    }
}

/// Managed append-or-replace text block for Markdown instruction files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedTextPatch {
    marker: String,
    content: String,
}

impl ManagedTextPatch {
    #[allow(dead_code)]
    pub(crate) fn new(marker: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            marker: marker.into(),
            content: content.into(),
        }
    }
}

pub(crate) fn render_managed_file(
    existing: Option<&str>,
    file: &ManagedFile,
) -> io::Result<String> {
    match &file.patch {
        ManagedFilePatch::Json(patch) => render_json(existing, patch),
        ManagedFilePatch::Toml(patch) => render_toml(existing, patch),
        ManagedFilePatch::Text(patch) => Ok(render_text(existing, patch)),
    }
}

fn render_json(existing: Option<&str>, patch: &JsonPatch) -> io::Result<String> {
    let mut root = parse_json_object(existing)?;

    for op in &patch.ops {
        let already_present = json_path_exists(&root, &op.path);
        if op.mode == PatchMode::IfMissing && already_present {
            continue;
        }
        set_json_path(&mut root, &op.path, op.value.clone())?;
    }

    serde_json::to_string_pretty(&root)
        .map(|rendered| format!("{rendered}\n"))
        .map_err(invalid_data)
}

fn parse_json_object(existing: Option<&str>) -> io::Result<Value> {
    let Some(existing) = existing else {
        return Ok(Value::Object(Map::new()));
    };

    let trimmed = existing.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Map::new()));
    }

    let parsed: Value =
        serde_json::from_str(&strip_json_comments(trimmed)).map_err(invalid_data)?;
    match parsed {
        Value::Object(_) => Ok(parsed),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected JSON object at config root",
        )),
    }
}

fn json_path_exists(root: &Value, path: &[String]) -> bool {
    let mut current = root;
    for segment in path {
        match current {
            Value::Object(object) => match object.get(segment) {
                Some(next) => current = next,
                None => return false,
            },
            _ => return false,
        }
    }
    true
}

fn set_json_path(root: &mut Value, path: &[String], value: Value) -> io::Result<()> {
    if path.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "JSON patch path must not be empty",
        ));
    }

    let mut current = root;
    for segment in &path[..path.len() - 1] {
        let object = current.as_object_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("JSON path segment '{segment}' requires an object"),
            )
        })?;
        let entry = object
            .entry(segment.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("JSON path segment '{segment}' collides with a non-object value"),
            ));
        }
        current = entry;
    }

    let object = current.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "JSON patch parent is not an object",
        )
    })?;
    object.insert(path[path.len() - 1].clone(), value);
    Ok(())
}

fn render_toml(existing: Option<&str>, patch: &TomlPatch) -> io::Result<String> {
    let mut document = if let Some(existing) = existing {
        let trimmed = existing.trim();
        if trimmed.is_empty() {
            DocumentMut::new()
        } else {
            trimmed.parse::<DocumentMut>().map_err(invalid_data)?
        }
    } else {
        DocumentMut::new()
    };

    for op in &patch.ops {
        if op.mode == PatchMode::IfMissing {
            match toml_path_state(document.as_item(), &op.path) {
                TomlPathState::Exists => continue,
                TomlPathState::Missing => {}
                TomlPathState::Blocked(segment) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("TOML path segment '{segment}' collides with a non-table value"),
                    ));
                }
            }
        }
        set_toml_path(&mut document, &op.path, &op.value)?;
    }

    let mut rendered = document.to_string();
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TomlPathState {
    Exists,
    Missing,
    Blocked(String),
}

fn toml_path_state(root: &Item, path: &[String]) -> TomlPathState {
    let mut current = root;
    for (index, segment) in path.iter().enumerate() {
        match current.as_table_like() {
            Some(table) => match table.get(segment) {
                Some(next) => current = next,
                None => return TomlPathState::Missing,
            },
            None => {
                let blocked_at = if index == 0 {
                    segment.clone()
                } else {
                    path[index - 1].clone()
                };
                return TomlPathState::Blocked(blocked_at);
            }
        }
    }
    TomlPathState::Exists
}

fn set_toml_path(document: &mut DocumentMut, path: &[String], value: &TomlValue) -> io::Result<()> {
    if path.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TOML patch path must not be empty",
        ));
    }

    let mut current = document.as_item_mut();
    for segment in &path[..path.len() - 1] {
        let table_like = current.as_table_like_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("TOML path segment '{segment}' requires a table"),
            )
        })?;

        let entry = table_like
            .entry(segment)
            .or_insert(Item::Table(Table::new()));
        if !entry.is_table_like() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("TOML path segment '{segment}' collides with a non-table value"),
            ));
        }
        current = entry;
    }

    let table_like = current.as_table_like_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "TOML patch parent is not a table",
        )
    })?;
    table_like.insert(&path[path.len() - 1], Item::Value(value.to_toml_edit()));
    Ok(())
}

fn render_text(existing: Option<&str>, patch: &ManagedTextPatch) -> String {
    let start = format!("<!-- mentisdb:{}:start -->", patch.marker);
    let end = format!("<!-- mentisdb:{}:end -->", patch.marker);
    let block = format!("{start}\n{}\n{end}\n", patch.content.trim_end());

    match existing {
        Some(existing) => {
            if let Some(start_index) = existing.find(&start) {
                if let Some(end_index) = existing.find(&end) {
                    let before = &existing[..start_index];
                    let after = &existing[end_index + end.len()..];
                    let mut rendered =
                        format!("{}{}{}", before.trim_end(), separator(before), block);
                    rendered.push_str(after.trim_start_matches('\n'));
                    return ensure_trailing_newline(rendered);
                }
            }

            if existing.contains(block.trim()) {
                return ensure_trailing_newline(existing.to_owned());
            }

            ensure_trailing_newline(format!(
                "{}{}{}",
                existing.trim_end(),
                separator(existing),
                block
            ))
        }
        None => block,
    }
}

fn separator(existing: &str) -> &'static str {
    if existing.trim().is_empty() {
        ""
    } else {
        "\n\n"
    }
}

fn ensure_trailing_newline(mut value: String) -> String {
    if !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

fn strip_json_comments(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            output.push(ch);
            continue;
        }

        if ch == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\n' {
                            output.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut previous = '\0';
                    for next in chars.by_ref() {
                        if previous == '*' && next == '/' {
                            break;
                        }
                        previous = next;
                    }
                    continue;
                }
                _ => {}
            }
        }

        output.push(ch);
    }

    output
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
