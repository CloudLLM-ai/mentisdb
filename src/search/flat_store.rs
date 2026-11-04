//! Flat, aligned, append-only vector storage shared by every consumer of a
//! sidecar's vectors.
//!
//! A [`VectorStore`] keeps vectors in fixed-size chunks of [`CHUNK_ROWS`] rows.
//! Each row is padded to whole 64-byte [`Line`]s (16 `f32`) and every chunk is a
//! single 64-byte-aligned allocation, so a chunk is a dense `rows x stride`
//! matrix. A [`VectorRow`] is a cheap handle (a chunk `Arc` plus a row number)
//! that dereferences to the row's `f32` values. The sidecar entries, the exact
//! index, the HNSW graph and the implicit-edge scanner all hold `VectorRow`s into
//! the same chunks, so the vectors exist once in memory.
//!
//! # Scoring
//!
//! [`dot`] multiplies and adds 16 independent lanes per line and reduces them in
//! a fixed order. Lane-wise `f32` arithmetic is what the compiler turns into
//! NEON on aarch64 and SSE2 (AVX2 when the build enables it) on x86_64, with no
//! `unsafe`, no nightly and no runtime feature probing. The operations and their
//! order do not depend on the target, so the result is the same on every
//! platform. It differs from a left-to-right sum by float reassociation only:
//! see [`DOT_EPSILON`].
//!
//! # Soundness of the append-in-place chunks
//!
//! A chunk is allocated whole and rows are written once, in order, by the single
//! [`VectorStore`] that owns the chunk (`&mut self` on [`VectorStore::push`]); a
//! [`VectorRow`] only exists for a row that has been fully written, and a row is
//! never written again. Readers therefore only ever touch rows nobody writes,
//! and the writer only touches rows nobody has a handle to, so no memory is read
//! and written concurrently. The handle reaches another thread through ordinary
//! synchronization (`Arc`, channels, locks), which orders the write before the
//! read. This is the one `unsafe` in the module; see `Chunk`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::UnsafeCell;
use std::fmt;
use std::sync::Arc;

/// `f32` values per 64-byte line.
pub const LINE_FLOATS: usize = 16;

/// Rows per chunk (about 196 KB at 384 dimensions).
pub const CHUNK_ROWS: usize = 128;

/// Largest absolute difference [`dot`] can have from a left-to-right
/// `f32` dot product of two unit vectors of up to 4096 dimensions. Cosine scores
/// built on it share this bound, which is far below the spacing of scores that
/// an embedding model separates.
pub const DOT_EPSILON: f32 = 1e-5;

/// Sixteen `f32`, aligned to a cache line.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct Line(pub [f32; LINE_FLOATS]);

impl Line {
    const ZERO: Line = Line([0.0; LINE_FLOATS]);
}

/// Number of lines one row of `dimension` values occupies (at least one).
pub fn lines_for(dimension: usize) -> usize {
    dimension.div_ceil(LINE_FLOATS).max(1)
}

/// A dense block of rows. See the module docs for why interior mutability is
/// sound here.
pub(crate) struct Chunk {
    dimension: usize,
    stride: usize,
    cells: Box<[UnsafeCell<Line>]>,
}

// SAFETY: see the module docs. Rows are written once by the owning store before
// any handle to them exists and are never written afterwards, so shared access
// from several threads only reads immutable memory.
unsafe impl Sync for Chunk {}
// SAFETY: the chunk owns plain `f32` data.
unsafe impl Send for Chunk {}
// `UnsafeCell` opts a type out of `RefUnwindSafe`, which would make every type
// holding a vector (the index, the sidecar, the HNSW backend) stop being
// `UnwindSafe` for callers that use `catch_unwind`. A panic cannot leave a
// chunk half-updated where a reader can see it: published rows are never
// written, and an unpublished row has no handle.
impl std::panic::RefUnwindSafe for Chunk {}

impl Chunk {
    fn new(dimension: usize, rows: usize) -> Arc<Self> {
        let stride = lines_for(dimension);
        let cells = (0..rows * stride)
            .map(|_| UnsafeCell::new(Line::ZERO))
            .collect();
        Arc::new(Self {
            dimension,
            stride,
            cells,
        })
    }

    /// The lines of `row`.
    fn row_lines(&self, row: usize) -> &[Line] {
        let first = row * self.stride;
        let cells = &self.cells[first..first + self.stride];
        // SAFETY: `UnsafeCell<Line>` has the layout of `Line`; the row was fully
        // written before a handle to it existed and is never written again.
        unsafe { std::slice::from_raw_parts(cells.as_ptr().cast::<Line>(), self.stride) }
    }

    /// The `dimension` values of `row`.
    fn row_values(&self, row: usize) -> &[f32] {
        let lines = self.row_lines(row);
        // SAFETY: `Line` is `repr(C)` over `[f32; 16]`, so `stride` lines are
        // `stride * 16` contiguous `f32`, of which the first `dimension` are the
        // vector.
        unsafe { std::slice::from_raw_parts(lines.as_ptr().cast::<f32>(), self.dimension) }
    }

    /// Write `values` into `row`.
    ///
    /// # Safety
    ///
    /// No handle to `row` may exist and no other thread may be writing it: the
    /// caller is the single owner of the chunk appending past its published rows.
    unsafe fn write_row(&self, row: usize, values: &[f32]) {
        let first = row * self.stride;
        let cells = &self.cells[first..first + self.stride];
        // SAFETY: per the contract, exclusive access to these cells.
        let destination = unsafe {
            std::slice::from_raw_parts_mut(
                UnsafeCell::raw_get(cells.as_ptr()).cast::<f32>(),
                self.stride * LINE_FLOATS,
            )
        };
        destination[..values.len()].copy_from_slice(values);
    }
}

/// A handle to one vector in a [`VectorStore`].
///
/// Cloning copies a pointer. It dereferences to the vector's values and
/// serializes as a plain sequence of `f32`, exactly like a `Vec<f32>`, so
/// persisted files that held vectors are unchanged.
#[derive(Clone)]
pub struct VectorRow {
    chunk: Arc<Chunk>,
    row: u32,
}

impl VectorRow {
    /// A row that owns a one-row chunk of its own: how vectors enter the system
    /// before a store adopts them.
    pub fn standalone(values: &[f32]) -> Self {
        let chunk = Chunk::new(values.len(), 1);
        // SAFETY: a fresh chunk nobody else can see.
        unsafe { chunk.write_row(0, values) };
        Self { chunk, row: 0 }
    }

    /// The row padded to whole [`Line`]s, for [`dot`].
    pub fn lines(&self) -> &[Line] {
        self.chunk.row_lines(self.row as usize)
    }

    /// Whether the row lives in a chunk of a [`VectorStore`] (many rows per
    /// allocation) rather than in a one-row chunk of its own.
    pub fn is_packed(&self) -> bool {
        self.chunk.cells.len() > self.chunk.stride
    }

    /// Whether both handles point at the same stored row.
    pub fn same_row(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.chunk, &other.chunk) && self.row == other.row
    }

    /// Number of handles (all kinds) to this row's chunk. For tests that prove a
    /// consumer shares storage instead of copying.
    #[doc(hidden)]
    pub fn chunk_handles(&self) -> usize {
        Arc::strong_count(&self.chunk)
    }
}

impl std::ops::Deref for VectorRow {
    type Target = [f32];

    fn deref(&self) -> &[f32] {
        self.chunk.row_values(self.row as usize)
    }
}

impl fmt::Debug for VectorRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl PartialEq for VectorRow {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl From<Vec<f32>> for VectorRow {
    fn from(values: Vec<f32>) -> Self {
        Self::standalone(&values)
    }
}

impl From<&[f32]> for VectorRow {
    fn from(values: &[f32]) -> Self {
        Self::standalone(values)
    }
}

impl Serialize for VectorRow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for VectorRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<f32>::deserialize(deserializer).map(|values| Self::standalone(&values))
    }
}

/// Append-only owner of the chunks a family of [`VectorRow`]s lives in.
///
/// Cloning a store shares the chunks it already has but gives the clone fresh
/// chunks for anything it appends, so two stores never write the same chunk.
#[derive(Default)]
pub struct VectorStore {
    dimension: usize,
    chunks: Vec<Arc<Chunk>>,
    /// Rows written into the last chunk, when this store owns it.
    tail_rows: usize,
    owns_tail: bool,
}

impl VectorStore {
    /// An empty store for vectors of `dimension` values.
    pub fn new(dimension: usize) -> Self {
        Self {
            dimension,
            chunks: Vec::new(),
            tail_rows: 0,
            owns_tail: false,
        }
    }

    /// Copy `values` into the store and return a handle to the new row.
    ///
    /// # Panics
    ///
    /// Panics if `values.len()` is not the store's dimension.
    pub fn push(&mut self, values: &[f32]) -> VectorRow {
        assert_eq!(values.len(), self.dimension, "vector dimension");
        if !self.owns_tail || self.tail_rows == CHUNK_ROWS {
            self.chunks.push(Chunk::new(self.dimension, CHUNK_ROWS));
            self.tail_rows = 0;
            self.owns_tail = true;
        }
        let chunk = self.chunks.last().expect("a chunk exists");
        // SAFETY: this store owns the tail chunk (`owns_tail`) and `&mut self`
        // makes it the only writer; row `tail_rows` has not been handed out.
        unsafe { chunk.write_row(self.tail_rows, values) };
        let row = VectorRow {
            chunk: Arc::clone(chunk),
            row: self.tail_rows as u32,
        };
        self.tail_rows += 1;
        row
    }

    /// Rows written into this store (live or not).
    pub fn row_count(&self) -> usize {
        match self.chunks.len() {
            0 => 0,
            n => (n - 1) * CHUNK_ROWS + self.tail_rows,
        }
    }

    /// Whether this store holds vectors of `dimension` values.
    pub fn has_dimension(&self, dimension: usize) -> bool {
        self.dimension == dimension
    }

    /// Whether `row` lives in a chunk of this store.
    pub fn contains(&self, row: &VectorRow) -> bool {
        self.chunks
            .iter()
            .any(|chunk| Arc::ptr_eq(chunk, &row.chunk))
    }

    /// The row if it already lives here, otherwise a copy of it in this store.
    pub fn adopt(&mut self, row: &VectorRow) -> VectorRow {
        if self.contains(row) {
            row.clone()
        } else {
            self.push(row)
        }
    }
}

impl Clone for VectorStore {
    fn clone(&self) -> Self {
        Self {
            dimension: self.dimension,
            chunks: self.chunks.clone(),
            tail_rows: 0,
            owns_tail: false,
        }
    }
}

/// Stores never take part in the equality of the structures that carry them:
/// they are where the vectors live, not what they are.
impl PartialEq for VectorStore {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl fmt::Debug for VectorStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VectorStore")
            .field("dimension", &self.dimension)
            .field("chunks", &self.chunks.len())
            .finish()
    }
}

/// Dot product of two rows already padded to whole lines.
///
/// Sixteen independent lane accumulators, reduced pairwise in a fixed order.
/// See the module docs for the precision contract ([`DOT_EPSILON`]).

///
/// # Examples
///
/// ```
/// use mentisdb::search::flat_store::{dot, VectorRow};
///
/// let a = VectorRow::standalone(&[1.0, 2.0, 3.0]);
/// let b = VectorRow::standalone(&[4.0, 5.0, 6.0]);
/// assert_eq!(dot(a.lines(), b.lines()), 32.0);
/// ```
pub fn dot(left: &[Line], right: &[Line]) -> f32 {
    let mut lanes = [0.0_f32; LINE_FLOATS];
    for (left, right) in left.iter().zip(right) {
        for ((lane, left), right) in lanes.iter_mut().zip(&left.0).zip(&right.0) {
            *lane += left * right;
        }
    }
    let mut width = LINE_FLOATS;
    while width > 1 {
        width /= 2;
        for lane in 0..width {
            lanes[lane] += lanes[lane + width];
        }
    }
    lanes[0]
}

/// Euclidean norm of a padded row, computed with [`dot`].
pub fn norm(lines: &[Line]) -> f32 {
    dot(lines, lines).sqrt()
}

/// A query padded to whole lines, ready to be scored against many rows.
pub struct PaddedQuery {
    lines: Vec<Line>,
    norm: f32,
}

impl PaddedQuery {
    /// Pad `values` (any length) to lines.
    pub fn new(values: &[f32]) -> Self {
        let row = VectorRow::standalone(values);
        let lines = row.lines().to_vec();
        let norm = norm(&lines);
        Self { lines, norm }
    }

    /// The query's Euclidean norm.
    pub fn norm(&self) -> f32 {
        self.norm
    }

    /// The padded lines.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo(seed: u64, n: usize) -> Vec<f32> {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((state >> 40) as f32 / (1u64 << 24) as f32) - 0.5
            })
            .collect()
    }

    /// Rows round-trip through the store, chunk boundaries included, and a
    /// handle reads the same values after more rows were appended.
    #[test]
    fn test_rows_round_trip_across_chunks() {
        let mut store = VectorStore::new(5);
        let rows: Vec<(Vec<f32>, VectorRow)> = (0..(CHUNK_ROWS * 2 + 7) as u64)
            .map(|i| {
                let values = pseudo(i, 5);
                let row = store.push(&values);
                (values, row)
            })
            .collect();
        for (values, row) in &rows {
            assert_eq!(&**row, values.as_slice());
        }
    }

    /// Chunks are cache-line aligned and rows are line-aligned.
    #[test]
    fn test_rows_are_64_byte_aligned() {
        let mut store = VectorStore::new(384);
        for i in 0..(CHUNK_ROWS + 3) as u64 {
            let row = store.push(&pseudo(i, 384));
            assert_eq!(row.lines().as_ptr() as usize % 64, 0);
            assert_eq!(row.lines().len(), 24);
        }
    }

    /// The lane dot product stays within `DOT_EPSILON` of a sequential sum on
    /// unit vectors of several dimensions, including ones that are not a
    /// multiple of the line width.
    #[test]
    fn test_dot_matches_a_sequential_sum_within_epsilon() {
        for dimension in [1, 3, 15, 16, 17, 100, 384, 768, 1536] {
            for seed in 0..20 {
                let unit = |s| {
                    let v = pseudo(s, dimension);
                    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                    v.iter().map(|x| x / n).collect::<Vec<f32>>()
                };
                let (a, b) = (unit(seed), unit(seed + 1000));
                let sequential: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
                let (ra, rb) = (VectorRow::standalone(&a), VectorRow::standalone(&b));
                let lanes = dot(ra.lines(), rb.lines());
                assert!((lanes - sequential).abs() <= DOT_EPSILON, "{dimension}d");
            }
        }
    }

    /// A cloned store appends to chunks of its own: the original's rows are not
    /// disturbed and the two never write the same chunk.
    #[test]
    fn test_a_cloned_store_never_writes_the_original_chunk() {
        let mut original = VectorStore::new(4);
        let first = original.push(&[1.0, 2.0, 3.0, 4.0]);
        let mut copy = original.clone();
        let from_copy = copy.push(&[9.0, 9.0, 9.0, 9.0]);
        let second = original.push(&[5.0, 6.0, 7.0, 8.0]);
        assert_eq!(&*first, &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(&*second, &[5.0, 6.0, 7.0, 8.0]);
        assert_eq!(&*from_copy, &[9.0, 9.0, 9.0, 9.0]);
        assert!(!original.contains(&from_copy));
        assert!(copy.contains(&first));
    }

    /// Handles move to other threads and read concurrently with the writer
    /// appending later rows to the same chunk.
    #[test]
    fn test_readers_on_other_threads_while_the_store_appends() {
        let mut store = VectorStore::new(8);
        let early: Vec<(Vec<f32>, VectorRow)> = (0..40u64)
            .map(|i| {
                let v = pseudo(i, 8);
                let r = store.push(&v);
                (v, r)
            })
            .collect();
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let early = early.clone();
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        for (values, row) in &early {
                            assert_eq!(&**row, values.as_slice());
                        }
                    }
                })
            })
            .collect();
        for i in 40..100u64 {
            store.push(&pseudo(i, 8));
        }
        for reader in readers {
            reader.join().unwrap();
        }
    }
}
