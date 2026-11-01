//! Public and private segments of a chain, and who may read them.
//!
//! A chain is **public** (anyone with access may write unsigned and anyone may
//! read) or **private** (writes need a signature by an authorized key, and the
//! thoughts written while it is private are readable only by a reader who
//! proves possession of an authorized key). The mode is not a flag: it is the
//! history of the chain's governance records. Every valid access-policy
//! record (see [`crate::access`]) is a **mode transition entry**. It opens a
//! new [`Segment`] that lasts until the next valid record, and the record
//! itself is the first thought of the segment it opens.
//!
//! ```text
//! index:   0 ......... 9 | 10 ........ 49 | 50 ......
//! record:  (none)        | seq0 allowlist | seq1 open
//! segment: public #0     | private #1     | public #2
//! ```
//!
//! Each thought belongs to exactly one segment, decided only by its position.
//! Nothing here is stored separately: the segment list is recomputed from the
//! chain by [`crate::access::AccessPolicy::replay`] whenever a chain opens, so
//! a chain that predates this model is migrated simply by being read. A chain
//! without records is one public segment. A chain whose policy was already
//! `signed` or `allowlist` is private from the position of that record.
//!
//! Two readers exist, expressed as an [`Audience`]:
//!
//! * [`Audience::Authorized`] sees every segment.
//! * [`Audience::Public`] sees only thoughts of public segments.
//!
//! The library default is [`Audience::Authorized`] because an embedder that
//! holds the chain directory already holds every byte of it. The server runs
//! every request with the audience its caller proved (see
//! `docs/CHAIN_ACCESS.md`) and starts from [`Audience::Public`], so forgetting
//! to decide is the safe mistake.
//!
//! # Examples
//!
//! ```
//! use mentisdb::audience::{Audience, Segment, SegmentKind, SegmentMap};
//!
//! let map = SegmentMap::from_segments(vec![
//!     Segment { start_index: 10, kind: SegmentKind::Private, seq: 0 },
//!     Segment { start_index: 51, kind: SegmentKind::Public, seq: 1 },
//! ]);
//! assert!(map.is_visible(9, Audience::Public));
//! assert!(!map.is_visible(10, Audience::Public));
//! assert!(!map.is_visible(50, Audience::Public));
//! assert!(map.is_visible(51, Audience::Public));
//! assert!(map.is_visible(10, Audience::Authorized));
//! ```

use serde::{Deserialize, Serialize};

/// Who is reading, as far as the segments of a chain are concerned.
///
/// # Examples
///
/// ```
/// use mentisdb::audience::Audience;
///
/// assert_eq!(Audience::default(), Audience::Authorized);
/// assert_eq!(Audience::Public.as_str(), "public");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// A reader who proved possession of an authorized key, or the operator
    /// (local filesystem access). Sees every segment.
    #[default]
    Authorized,
    /// Everyone else. Sees only thoughts that belong to public segments.
    Public,
}

impl Audience {
    /// Lowercase label used in responses and logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authorized => "authorized",
            Self::Public => "public",
        }
    }

    /// `true` for [`Audience::Public`].
    #[must_use]
    pub fn is_public(self) -> bool {
        self == Self::Public
    }
}

/// Whether a segment is open to everyone or reserved for authorized readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    /// Written while the chain accepted unsigned writes; readable by everyone.
    Public,
    /// Written while the chain required signatures; readable only by
    /// [`Audience::Authorized`].
    Private,
}

impl SegmentKind {
    /// Lowercase label used in responses and logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

/// One stretch of a chain that is entirely public or entirely private.
///
/// A private segment starts at the governance record that opened it. A public
/// segment starts at the thought after the record that closed the private one
/// (the record itself stays private). Either lasts until the next valid
/// record. The implicit first segment of a chain (everything before any
/// record) is public and is not listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Segment {
    /// Index of the first thought of the segment.
    pub start_index: u64,
    /// Whether the segment is public or private.
    pub kind: SegmentKind,
    /// `seq` of the governance record that opened the segment.
    pub seq: u64,
}

/// The ordered segments of one chain, with fast position lookup.
///
/// Built by [`crate::access::AccessPolicy::replay`] and kept up to date by
/// [`crate::MentisDb::set_access_policy`]. Lookups are `O(log n)` in the
/// number of mode transitions, which is tiny.
///
/// # Examples
///
/// ```
/// use mentisdb::audience::{Segment, SegmentKind, SegmentMap};
///
/// let empty = SegmentMap::default();
/// assert!(!empty.has_private());
/// assert_eq!(empty.kind_at(123), SegmentKind::Public);
///
/// let map = SegmentMap::from_segments(vec![Segment {
///     start_index: 3,
///     kind: SegmentKind::Private,
///     seq: 0,
/// }]);
/// assert!(map.has_private());
/// assert_eq!(map.kind_at(2), SegmentKind::Public);
/// assert_eq!(map.kind_at(3), SegmentKind::Private);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SegmentMap {
    segments: Vec<Segment>,
    /// Indices of valid governance records, in order. Always private.
    records: Vec<u64>,
    /// Thoughts that failed verification when the chain opened (unsigned or
    /// forged in a private segment, invalid governance records). Hidden from
    /// every reader and listed in the verification report. Sorted.
    #[serde(default)]
    quarantined: Vec<u64>,
    /// Set when a signed checkpoint proves the chain file no longer matches
    /// what was verified (records removed or rewritten): every position reads as
    /// private, so the chain is served to nobody until the operator re-seals it.
    #[serde(default)]
    locked: bool,
}

impl SegmentMap {
    /// Build a map from segments; they are sorted by `start_index` and a later
    /// segment replaces an earlier one that starts at the same index.
    #[must_use]
    pub fn from_segments(mut segments: Vec<Segment>) -> Self {
        segments.sort_by_key(|segment| segment.start_index);
        segments.reverse();
        segments.dedup_by_key(|segment| segment.start_index);
        segments.reverse();
        Self {
            segments,
            records: Vec::new(),
            quarantined: Vec::new(),
            locked: false,
        }
    }

    /// Mark the chain as diverged from its verified state (see the field docs).
    pub fn lock(&mut self) {
        self.locked = true;
    }

    /// Whether the chain is locked.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Record that the thought at `index` is a valid governance record. Such a
    /// thought is private whatever segment its position falls in.
    pub fn push_record(&mut self, index: u64) {
        if self.records.last().is_none_or(|last| index > *last) {
            self.records.push(index);
        }
    }

    /// Replace the quarantine list (sorted and de-duplicated here).
    pub fn set_quarantine(&mut self, mut indices: Vec<u64>) {
        indices.sort_unstable();
        indices.dedup();
        self.quarantined = indices;
    }

    /// Indices quarantined by open-time verification.
    #[must_use]
    pub fn quarantined(&self) -> &[u64] {
        &self.quarantined
    }

    /// The same map without the quarantine list, for readers who must not learn
    /// which positions failed verification.
    #[must_use]
    pub fn without_quarantine(&self) -> Self {
        Self {
            quarantined: Vec::new(),
            ..self.clone()
        }
    }

    /// Indices of the valid governance records.
    #[must_use]
    pub fn records(&self) -> &[u64] {
        &self.records
    }

    /// The listed segments in position order (the implicit leading public
    /// segment is not included).
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Append a segment that starts at or after the last one.
    pub fn push(&mut self, segment: Segment) {
        if let Some(last) = self.segments.last() {
            if segment.start_index < last.start_index {
                return;
            }
            if segment.start_index == last.start_index {
                self.segments.pop();
            }
        }
        self.segments.push(segment);
    }

    /// `true` when any thought of the chain can be private, now or earlier.
    ///
    /// When this is `false` every reader is equivalent and no filtering is
    /// needed.
    #[must_use]
    pub fn has_private(&self) -> bool {
        self.locked
            || !self.records.is_empty()
            || !self.quarantined.is_empty()
            || self
                .segments
                .iter()
                .any(|segment| segment.kind == SegmentKind::Private)
    }

    /// Kind of the segment the thought at `index` belongs to.
    #[must_use]
    pub fn kind_at(&self, index: u64) -> SegmentKind {
        if self.locked {
            return SegmentKind::Private;
        }
        if self.records.binary_search(&index).is_ok() {
            return SegmentKind::Private;
        }
        match self
            .segments
            .partition_point(|segment| segment.start_index <= index)
        {
            0 => SegmentKind::Public,
            n => self.segments[n - 1].kind,
        }
    }

    /// Whether `audience` may see the thought at `index`.
    #[must_use]
    pub fn is_visible(&self, index: u64, audience: Audience) -> bool {
        if self.locked {
            return false;
        }
        if !self.quarantined.is_empty() && self.quarantined.binary_search(&index).is_ok() {
            return false;
        }
        match audience {
            Audience::Authorized => true,
            Audience::Public => self.kind_at(index) == SegmentKind::Public,
        }
    }
}
