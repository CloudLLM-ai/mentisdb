//! Cross-platform path resolution helpers for MentisDB and host integrations.
//!
//! The setup and wizard flows need deterministic, testable rules for common
//! user-level configuration directories. This module centralizes those rules so
//! daemon code, CLI code, and setup planning share the same path semantics.

use std::path::{Path, PathBuf};

const MENTISDB_DIRNAME: &str = "mentisdb";

/// Operating-system family used for integration path planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostPlatform {
    /// Apple macOS.
    Macos,
    /// Linux and other Unix systems that follow XDG-style config roots.
    Linux,
    /// Microsoft Windows.
    Windows,
    /// Any unsupported or unknown platform.
    Other,
}

impl HostPlatform {
    /// Return the host platform for the current process.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }

    /// Return a stable lowercase identifier suitable for logs and JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }
}

/// Snapshot of environment-derived path inputs used to resolve config roots.
///
/// This struct makes platform path logic deterministic in tests and reusable by
/// future CLI/setup flows without depending directly on global process state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathEnvironment {
    /// Explicit override for the MentisDB storage root (`MENTISDB_DIR`).
    pub mentisdb_dir_override: Option<PathBuf>,
    /// POSIX-style home directory (`HOME`).
    pub home_dir: Option<PathBuf>,
    /// Windows-style profile root (`USERPROFILE`).
    pub user_profile: Option<PathBuf>,
    /// XDG config directory (`XDG_CONFIG_HOME`).
    pub xdg_config_home: Option<PathBuf>,
    /// Windows roaming application data directory (`APPDATA`).
    pub app_data: Option<PathBuf>,
    /// Windows local application data directory (`LOCALAPPDATA`).
    pub local_app_data: Option<PathBuf>,
    /// Current working directory used as the last fallback.
    pub current_dir: Option<PathBuf>,
}

impl PathEnvironment {
    /// Capture relevant path inputs from the current process environment.
    pub fn capture() -> Self {
        Self {
            mentisdb_dir_override: non_empty_var("MENTISDB_DIR"),
            home_dir: non_empty_var("HOME"),
            user_profile: non_empty_var("USERPROFILE"),
            xdg_config_home: non_empty_var("XDG_CONFIG_HOME"),
            app_data: non_empty_var("APPDATA"),
            local_app_data: non_empty_var("LOCALAPPDATA"),
            current_dir: std::env::current_dir().ok(),
        }
    }

    /// Return the most appropriate home directory for a platform.
    pub fn home_dir_for(&self, platform: HostPlatform) -> Option<PathBuf> {
        match platform {
            HostPlatform::Windows => self.user_profile.clone().or_else(|| self.home_dir.clone()),
            _ => self.home_dir.clone().or_else(|| self.user_profile.clone()),
        }
    }

    /// Return the user-scoped configuration root for a platform.
    ///
    /// On macOS this is `~/Library/Application Support`, on Linux/XDG it is
    /// `XDG_CONFIG_HOME` or `~/.config`, and on Windows it is `APPDATA` when
    /// available.
    pub fn config_root_for(&self, platform: HostPlatform) -> Option<PathBuf> {
        match platform {
            HostPlatform::Macos => self
                .home_dir_for(platform)
                .map(|home| home.join("Library").join("Application Support")),
            HostPlatform::Linux => self
                .xdg_config_home
                .clone()
                .or_else(|| self.home_dir_for(platform).map(|home| home.join(".config"))),
            HostPlatform::Windows => self
                .app_data
                .clone()
                .or_else(|| self.local_app_data.clone())
                .or_else(|| {
                    self.home_dir_for(platform)
                        .map(|home| home.join("AppData").join("Roaming"))
                }),
            HostPlatform::Other => self
                .xdg_config_home
                .clone()
                .or_else(|| self.home_dir_for(platform).map(|home| home.join(".config"))),
        }
    }

    /// Resolve the default MentisDB storage directory using the current
    /// environment snapshot.
    pub fn default_mentisdb_dir(&self) -> PathBuf {
        if let Some(path) = self
            .mentisdb_dir_override
            .clone()
            .filter(|path| !path.as_os_str().is_empty())
        {
            return path;
        }

        if let Some(home) = self
            .home_dir
            .clone()
            .or_else(|| self.user_profile.clone())
            .filter(|path| !path.as_os_str().is_empty())
        {
            return home.join(".cloudllm").join(MENTISDB_DIRNAME);
        }

        self.current_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".cloudllm")
            .join(MENTISDB_DIRNAME)
    }
}

/// Resolve the default on-disk MentisDB storage directory for the current
/// process environment.
///
/// This follows the same priority chain as the daemon:
///
/// 1. `MENTISDB_DIR`
/// 2. `$HOME/.cloudllm/mentisdb` (or `%USERPROFILE%\.cloudllm\mentisdb`)
/// 3. `./.cloudllm/mentisdb`
pub fn default_mentisdb_dir() -> PathBuf {
    PathEnvironment::capture().default_mentisdb_dir()
}

/// A warning when the data directory `dir` exists and group or other users can
/// open it (Unix only).
///
/// MentisDB creates a data directory it makes itself with mode `0700` (and its
/// chain, registry, sidecar and backup files `0600`), but never changes the
/// mode of one it finds: a directory created earlier, by an older release or
/// by hand may be `0755`, and it holds plaintext thoughts, private chains and
/// backups. The text names the directory, its mode and the `chmod 700`
/// command that fixes it. The daemon logs it at startup.
///
/// Returns `None` when the directory is private or missing, and always on
/// platforms without Unix permission bits.
///
/// # Examples
///
/// ```
/// use mentisdb::paths::data_dir_mode_warning;
///
/// let missing = std::path::Path::new("/nonexistent/mentisdb-doc-example");
/// assert!(data_dir_mode_warning(missing).is_none());
/// ```
pub fn data_dir_mode_warning(dir: &std::path::Path) -> Option<String> {
    crate::private_fs::mode_warning(dir)
}

/// Why a `.env` file could not be loaded or rewritten.
///
/// Messages name the file and, when it can be found, the line, but never echo
/// a line's content: the lines of a settings file hold secrets such as
/// `MENTISDB_DASHBOARD_PIN`.
#[derive(Debug)]
pub enum EnvFileError {
    /// The file exists but could not be read (permissions, a directory in its
    /// place, bytes that are not UTF-8).
    Read {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The replacement file could not be written, synced or renamed into place.
    Write {
        /// The file being replaced.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The file holds a line the `.env` parser rejects. The parser stops at the
    /// first such line, so every setting after it, a PIN or the bearer-token
    /// switch among them, would be dropped.
    Parse {
        /// The file.
        path: PathBuf,
        /// 1-based number of the first line that does not parse, when found.
        line: Option<usize>,
    },
    /// A name that is not a valid variable name (`[A-Za-z_][A-Za-z0-9_]*`).
    InvalidName(String),
    /// A value that cannot be stored in a `.env` file.
    InvalidValue {
        /// The variable.
        name: String,
        /// What is wrong with the value.
        reason: EnvValueError,
    },
}

impl std::fmt::Display for EnvFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Write { path, source } => write!(f, "cannot write {}: {source}", path.display()),
            Self::Parse {
                path,
                line: Some(line),
            } => write!(
                f,
                "{} line {line} is not valid .env syntax (the line is not shown: it may hold a secret). \
                 Settings after it would be dropped, so the file must be fixed",
                path.display()
            ),
            Self::Parse { path, line: None } => write!(
                f,
                "{} is not valid .env syntax (for example an unterminated quote). \
                 Settings after the bad line would be dropped, so the file must be fixed",
                path.display()
            ),
            Self::InvalidName(name) => write!(f, "'{name}' is not a valid variable name"),
            Self::InvalidValue { name, reason } => write!(f, "value of {name} {reason}"),
        }
    }
}

impl std::error::Error for EnvFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A value that cannot be written to a `.env` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvValueError {
    /// Contains a NUL byte, which no environment variable can hold.
    Nul,
    /// Contains a carriage return or line feed, which would end the line and
    /// let the rest of the value be read as further settings.
    LineBreak,
}

impl std::fmt::Display for EnvValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Nul => "contains a NUL byte",
            Self::LineBreak => "contains a line break",
        })
    }
}

impl std::error::Error for EnvValueError {}

/// Render `value` as the right-hand side of a `NAME=value` line that the
/// `.env` loader reads back unchanged.
///
/// The loader (`dotenvy`) expands `$NAME` and `${NAME}` in unquoted and
/// double-quoted values, treats ` #` as a comment, rejects a value with a
/// space or a stray quote, and ends a single-quoted value at a backslash it
/// cannot see past, so a PIN such as `pa$word#1` written raw loads as `pa#1`
/// and `a b` aborts the whole file. A value made only of letters, digits and
/// `_ . / : @ % + , = -` is returned as is, which keeps an operator's file
/// readable. Anything else becomes a double-quoted string in which `\`, `"`
/// and `$` are backslash-escaped, the one form that can hold every other
/// character (spaces, `#`, `'`, non-ASCII) without surprises. An empty value
/// becomes `""`.
///
/// # Errors
///
/// [`EnvValueError`] for a NUL, carriage return or line feed.
///
/// # Examples
///
/// ```
/// use mentisdb::paths::dotenv_quote;
///
/// assert_eq!(dotenv_quote("/var/lib/mentisdb").unwrap(), "/var/lib/mentisdb");
/// assert_eq!(dotenv_quote("pa$word#1").unwrap(), r#""pa\$word#1""#);
/// assert_eq!(dotenv_quote("").unwrap(), r#""""#);
/// assert!(dotenv_quote("a\nB=c").is_err());
/// ```
pub fn dotenv_quote(value: &str) -> Result<String, EnvValueError> {
    if value.contains('\0') {
        return Err(EnvValueError::Nul);
    }
    if value.contains(['\n', '\r']) {
        return Err(EnvValueError::LineBreak);
    }
    let plain = |c: char| c.is_ascii_alphanumeric() || "_./:@%+,=-".contains(c);
    if !value.is_empty() && value.chars().all(plain) {
        return Ok(value.to_owned());
    }
    let escaped = value.chars().fold(String::from('"'), |mut out, c| {
        if matches!(c, '\\' | '"' | '$') {
            out.push('\\');
        }
        out.push(c);
        out
    });
    Ok(escaped + "\"")
}

/// Quoting state of the loader's line joiner, which decides where one
/// assignment ends: a quote left open carries it over onto the next line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scan {
    Complete,
    Space,
    Escape,
    Strong,
    StrongEscape,
    Weak,
    WeakEscape,
}

/// Advance the joiner over one physical line. Mirrors `dotenvy` 0.15's
/// `eval_end_state`, so that this module and the loader agree on what a
/// logical line is (a multi-line quoted value is one).
fn scan_line(state: Scan, line: &str) -> Scan {
    let mut state = state;
    for c in line.chars().chain(std::iter::once('\n')) {
        state = match (state, c) {
            (Scan::Space, '#') => return Scan::Complete,
            (Scan::Space | Scan::Complete, '\\') => Scan::Escape,
            (Scan::Space | Scan::Complete, '"') => Scan::Weak,
            (Scan::Space | Scan::Complete, '\'') => Scan::Strong,
            (Scan::Complete, c) if c.is_whitespace() && c != '\n' && c != '\r' => Scan::Space,
            (Scan::Space | Scan::Complete | Scan::Escape, _) => Scan::Complete,
            (Scan::Weak, '\\') => Scan::WeakEscape,
            (Scan::Weak, '"') => Scan::Complete,
            (Scan::Weak | Scan::WeakEscape, _) => Scan::Weak,
            (Scan::Strong, '\\') => Scan::StrongEscape,
            (Scan::Strong, '\'') => Scan::Complete,
            (Scan::Strong | Scan::StrongEscape, _) => Scan::Strong,
        };
    }
    state
}

/// Group the physical lines of `text` into the logical lines the loader sees:
/// each blank line, comment, or assignment (with any quoted continuation
/// lines) is one entry. A quote left open at the end of the file leaves the
/// last entry unterminated, as is.
fn split_entries(text: &str) -> Vec<Vec<&str>> {
    let mut entries = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut state = Scan::Complete;
    let lines = text.strip_suffix('\n').unwrap_or(text).split('\n');
    for raw in lines.filter(|_| !text.is_empty()) {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if current.is_empty() && line.trim_start().starts_with('#') {
            entries.push(vec![line]);
            continue;
        }
        state = scan_line(state, line);
        current.push(line);
        if state == Scan::Complete {
            entries.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        entries.push(current);
    }
    entries
}

fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The variable an entry assigns and whether it is written `export NAME=...`,
/// as the loader reads it: optional `export` and blanks, the name, optional
/// blanks, `=`. `None` for blanks, comments and anything else.
fn assigned_name(first_line: &str) -> Option<(&str, bool)> {
    let line = first_line.trim_start();
    let (line, exported) = match line.strip_prefix("export") {
        Some(rest) if rest.starts_with(char::is_whitespace) => (rest.trim_start(), true),
        _ => (line, false),
    };
    let end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(line.len());
    let (name, rest) = line.split_at(end);
    let starts_ok = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    (starts_ok && rest.trim_start().starts_with('=')).then_some((name, exported))
}

/// Apply `changes` to the text of a `.env` file and return the new text.
///
/// Each change is `(name, Some(value))` to set or `(name, None)` to remove.
/// Matching follows the loader: `NAME=`, `NAME =` and `export NAME=` all
/// assign `NAME`, and the loader keeps the first of several, so the first is
/// updated in place (keeping its `export`) and later ones are deleted, which
/// stops an old value from shadowing or outliving the new one. A name not in
/// the file is appended when it has a value. Comments, blank lines, other
/// variables and multi-line quoted values are kept as they are; values are
/// written with [`dotenv_quote`]. Names must be unique within `changes`.
///
/// # Errors
///
/// [`EnvFileError::InvalidName`] or [`EnvFileError::InvalidValue`]; nothing is
/// returned in that case, so a bad entry never produces a half-edited file.
///
/// # Examples
///
/// ```
/// use mentisdb::paths::edit_env_text;
///
/// let old = "# settings\nexport A=1\nB=2\nA=stale\n";
/// let new = edit_env_text(old, &[("A", Some("x y")), ("B", None), ("C", Some("3"))]).unwrap();
/// assert_eq!(new, "# settings\nexport A=\"x y\"\nC=3\n");
/// ```
pub fn edit_env_text(
    content: &str,
    changes: &[(&str, Option<&str>)],
) -> Result<String, EnvFileError> {
    let rendered = changes
        .iter()
        .map(|&(name, value)| {
            if !is_valid_env_name(name) {
                return Err(EnvFileError::InvalidName(name.to_owned()));
            }
            let value = value.map(dotenv_quote).transpose().map_err(|reason| {
                EnvFileError::InvalidValue {
                    name: name.to_owned(),
                    reason,
                }
            })?;
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bom = if content.starts_with('\u{feff}') {
        "\u{feff}"
    } else {
        ""
    };
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for entry in split_entries(body) {
        let target = assigned_name(entry[0]).and_then(|(name, exported)| {
            rendered
                .iter()
                .find(|(n, _)| *n == name)
                .map(|r| (r, exported))
        });
        match target {
            None => out.extend(entry.iter().map(|line| (*line).to_owned())),
            Some(((name, value), exported)) => {
                if seen.insert(*name) {
                    let export = if exported { "export " } else { "" };
                    out.extend(value.iter().map(|v| format!("{export}{name}={v}")));
                }
            }
        }
    }
    for (name, value) in &rendered {
        if !seen.contains(name) {
            out.extend(value.iter().map(|v| format!("{name}={v}")));
        }
    }
    let body = out
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    Ok(format!("{bom}{body}"))
}

/// Serialises read-modify-write cycles of [`update_env_file`] within the
/// process, so two saves cannot interleave and lose one another's change.
static ENV_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Edit the `.env` file at `path` with [`edit_env_text`], replacing it
/// atomically.
///
/// The new text goes to a temporary file in the same directory (mode `0600` on
/// Unix, since the file can hold the dashboard PIN), is synced, and is renamed
/// over the original, then the directory is synced: a crash leaves either the
/// old file or the new one, never a truncated one. A missing file is treated
/// as empty and created when a change has a value; a symlink is followed, so
/// the link survives. When the edit changes nothing, nothing is written.
///
/// An existing file that cannot be read is an error and is left untouched:
/// rewriting from an empty buffer would drop every setting the change did not
/// mention, the PIN included.
///
/// # Errors
///
/// [`EnvFileError::Read`] when the existing file cannot be read,
/// [`EnvFileError::Write`] when the replacement cannot be written, and the
/// name and value errors of [`edit_env_text`].
pub fn update_env_file(path: &Path, changes: &[(&str, Option<&str>)]) -> Result<(), EnvFileError> {
    let _guard = ENV_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let content = match std::fs::read_to_string(&target) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(EnvFileError::Read {
                path: target,
                source,
            })
        }
    };
    let updated = edit_env_text(&content, changes)?;
    if updated == content {
        return Ok(());
    }
    replace_file_atomically(&target, updated.as_bytes()).map_err(|source| EnvFileError::Write {
        path: target,
        source,
    })
}

/// Write `bytes` to `target` through a synced, owner-only temporary file and a
/// rename.
fn replace_file_atomically(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = dir.join(format!(".env.{}.{unique}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&temp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temp, target));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
        return written;
    }
    #[cfg(unix)]
    std::fs::File::open(dir).and_then(|d| d.sync_all())?;
    Ok(())
}

/// Path of the settings file the dashboard writes: `<MENTISDB_DIR>/.env`.
///
/// The daemon loads it at startup after any `.env` in the working
/// directory, so values saved from the dashboard Settings tab take effect on
/// restart regardless of where the daemon was launched from.
pub fn mentisdb_dir_env_file() -> PathBuf {
    default_mentisdb_dir().join(".env")
}

/// 1-based number of the first line of `text` that the `.env` parser rejects,
/// found by parsing each logical line on its own. Used only to point an
/// operator at the problem.
fn first_unparsable_line(text: &str) -> Option<usize> {
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut line_no = 1;
    for entry in split_entries(body) {
        let first = entry[0].trim_start();
        let parses = first.is_empty()
            || first.starts_with('#')
            || dotenvy::from_read_iter(entry.join("\n").as_bytes())
                .next()
                .is_none_or(|item| item.is_ok());
        if !parses {
            return Some(line_no);
        }
        line_no += entry.len();
    }
    None
}

/// Load one `.env` file into the process environment, never overriding a
/// variable that is already set.
///
/// Returns `Ok(false)` when the file does not exist and `Ok(true)` when it
/// was loaded.
///
/// # Errors
///
/// [`EnvFileError::Read`] when it exists but cannot be read, and
/// [`EnvFileError::Parse`] when a line is not valid `.env` syntax. The parser
/// stops at the first bad line, so the settings after it were not applied;
/// the caller should treat this as fatal rather than run with them missing.
pub fn load_dotenv_file(path: &Path) -> Result<bool, EnvFileError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(EnvFileError::Read {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    match dotenvy::from_read(bytes.as_slice()) {
        Ok(()) => Ok(true),
        Err(dotenvy::Error::Io(source)) => Err(EnvFileError::Read {
            path: path.to_path_buf(),
            source,
        }),
        Err(_) => Err(EnvFileError::Parse {
            path: path.to_path_buf(),
            line: first_unparsable_line(&String::from_utf8_lossy(&bytes)),
        }),
    }
}

/// The `.env` file the working directory resolves to: the first `.env` in the
/// current directory or any parent, as `dotenvy::dotenv` finds it.
fn find_working_dir_dotenv() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .map(|dir| dir.join(".env"))
        .find(|candidate| candidate.is_file())
}

/// Load `.env` files into the process environment at daemon startup.
///
/// Order (earlier wins, since already-set variables are never overridden):
/// the real process environment, then `./.env` in the working directory (or
/// the nearest parent that has one), then [`mentisdb_dir_env_file`]. The
/// working-directory file is read first so it may itself set `MENTISDB_DIR`.
/// Missing files are ignored. Returns the files that were loaded.
///
/// # Errors
///
/// Fails on a file that exists but cannot be read or is not valid `.env`
/// syntax (see [`load_dotenv_file`]). The daemon must not carry on: a file cut
/// short at its first bad line can silently drop `MENTISDB_DASHBOARD_PIN` or
/// `MENTISDB_BEARER_TOKEN_ACCESS`, and the daemon would start open.
pub fn try_load_dotenv_files() -> Result<Vec<PathBuf>, EnvFileError> {
    let mut loaded = Vec::new();
    if let Some(path) = find_working_dir_dotenv() {
        if load_dotenv_file(&path)? {
            loaded.push(path);
        }
    }
    let dir_env = mentisdb_dir_env_file();
    if !loaded.contains(&dir_env) && load_dotenv_file(&dir_env)? {
        loaded.push(dir_env);
    }
    Ok(loaded)
}

/// Load `.env` files like [`try_load_dotenv_files`], reporting a bad file on
/// stderr instead of failing.
///
/// Kept with its 0.12.2 signature for library callers. It loads every file
/// that parses and prints a warning naming any file that does not, which
/// matters because a file cut short at a bad line can silently drop security
/// settings such as `MENTISDB_DASHBOARD_PIN`. The daemon uses
/// [`try_load_dotenv_files`] and refuses to start instead; embedders that load
/// the same settings should do the same.
pub fn load_dotenv_files() -> Vec<PathBuf> {
    let working = find_working_dir_dotenv();
    let dir_env = mentisdb_dir_env_file();
    let skip_dir_env = working.as_ref() == Some(&dir_env);
    working
        .into_iter()
        .chain((!skip_dir_env).then_some(dir_env))
        .filter(|path| match load_dotenv_file(path) {
            Ok(found) => found,
            Err(error) => {
                eprintln!("warning: {error}");
                false
            }
        })
        .collect()
}

fn non_empty_var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}
