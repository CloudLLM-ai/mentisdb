//! Append-only snapshot store of sidecar vectors for off-lock neighbour scans.
//!
//! The implicit-edge overlay needs, for every newly applied vector, the cosine
//! similarity to every earlier vector: `O(entries x dimension)` (about 25 ms at
//! 100 000 x 384). Done under a chain's write lock that stalls every other
//! writer, so a service computes it on another thread from a *snapshot* of the
//! vectors. [`VectorScanStore`] is that snapshot source.
//!
//! # One copy of the vectors
//!
//! The store holds no floats. Each position is a clone of the
//! `Arc<[f32]>` of the sidecar entry it mirrors
//! ([`crate::search::VectorSidecarEntry::vector`]), so the sidecar and the
//! scanner read the same allocation: the store costs about 40 bytes per vector
//! (a fat pointer, the thought id and the thought index), against 1 536 bytes
//! per vector at 384 dimensions for a copy. The entries sit in sealed chunks
//! behind `Arc`, so taking a snapshot clones a short list of pointers
//! (`entries / 128` of them) and never copies a vector. The few vectors not yet
//! in a sealed chunk (fewer than [`SCAN_CHUNK_ENTRIES`]) are not in the
//! snapshot; the caller scans those under the lock with
//! [`VectorScanStore::scan_range`], which is the small "delta".
//!
//! Scores come from the sequential [`cosine_similarity`] on identical slices, so a
//! scan split between a snapshot and the live delta returns exactly the
//! candidates (same order, bit-identical scores) a single scan over the sidecar
//! would. Exact vector search scores with the lane-parallel
//! [`dot`](crate::search::flat_store::dot) instead; the same pair of vectors can
//! score differently on the two paths by up to
//! [`DOT_EPSILON`](crate::search::flat_store::DOT_EPSILON) (1e-5), which only
//! matters to a caller that compares a search score with the edge threshold.

use crate::search::flat_store::VectorRow;
use crate::search::implicit_edges::ImplicitNeighbor;
use crate::search::sidecar::{VectorSidecar, VectorSidecarEntry};
use crate::search::vector::cosine_similarity;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use uuid::Uuid;

/// Vectors per sealed chunk. Bounds the work a caller does under its lock
/// (the unsealed tail) to this many cosine comparisons per scanned vector.
pub(crate) const SCAN_CHUNK_ENTRIES: usize = 128;

/// Source of store identities, so a result computed against a store that was
/// rebuilt in the meantime can be recognised and discarded.
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

/// A run of up to [`SCAN_CHUNK_ENTRIES`] vectors, shared with the sidecar.
#[derive(Default)]
pub(crate) struct ScanChunk {
    ids: Vec<Uuid>,
    vectors: Vec<VectorRow>,
}

/// An immutable view of the sealed part of a [`VectorScanStore`].
#[derive(Clone)]
pub(crate) struct ScanSnapshot {
    /// Identity of the store the snapshot was taken from.
    pub(crate) epoch: u64,
    chunks: Vec<Arc<ScanChunk>>,
}

impl ScanSnapshot {
    /// Number of vectors in the snapshot.
    pub(crate) fn len(&self) -> usize {
        self.chunks.len() * SCAN_CHUNK_ENTRIES
    }
}

/// The vector a scan looks for neighbours of.
pub(crate) struct ScanQuery<'a> {
    /// Thought the vector belongs to; never its own neighbour.
    pub(crate) id: Uuid,
    pub(crate) vector: &'a [f32],
    /// Smallest cosine score that counts as a neighbour.
    pub(crate) threshold: f32,
}

/// One vector to scan for, and how much of the store precedes it.
pub(crate) struct ScanItem<'a> {
    pub(crate) query: ScanQuery<'a>,
    /// Number of stored vectors that precede it (those with a smaller thought
    /// index): the scan covers positions `0..prefix_len`.
    pub(crate) prefix_len: usize,
}

/// Append-only copy of a sidecar's vectors, in sidecar order.
pub(crate) struct VectorScanStore {
    epoch: u64,
    dimension: usize,
    sealed: Vec<Arc<ScanChunk>>,
    tail: ScanChunk,
    indices: Vec<u64>,
    last_hash: Option<String>,
}

impl VectorScanStore {
    /// An empty store for vectors of `dimension` components.
    pub(crate) fn new(dimension: usize) -> Self {
        Self {
            epoch: NEXT_EPOCH.fetch_add(1, Ordering::Relaxed),
            dimension,
            sealed: Vec::new(),
            tail: ScanChunk::default(),
            indices: Vec::new(),
            last_hash: None,
        }
    }

    /// Identity of this store; changes whenever it is rebuilt from scratch.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Vectors held.
    pub(crate) fn len(&self) -> usize {
        self.indices.len()
    }

    /// Thought index of the vector at `position`.
    pub(crate) fn index_at(&self, position: usize) -> Option<u64> {
        self.indices.get(position).copied()
    }

    /// Number of stored vectors whose thought index is below `index`.
    pub(crate) fn position_before(&self, index: u64) -> usize {
        self.indices.partition_point(|&stored| stored < index)
    }

    /// Bring the store in line with `sidecar`: append the entries it lacks, or
    /// start over (new epoch) when the sidecar is no longer an extension of
    /// what was copied (replaced by a rebuild, rewritten chain).
    pub(crate) fn sync_with(&mut self, sidecar: &VectorSidecar) {
        let held = self.len();
        let extends = held <= sidecar.entries.len()
            && self.dimension == sidecar.metadata.dimension
            && match held.checked_sub(1) {
                None => true,
                Some(last) => {
                    Some(sidecar.entries[last].thought_hash.as_str()) == self.last_hash.as_deref()
                }
            };
        if !extends {
            *self = Self::new(sidecar.metadata.dimension);
        }
        for entry in &sidecar.entries[self.len()..] {
            self.push(entry);
        }
    }

    fn push(&mut self, entry: &VectorSidecarEntry) {
        if self.tail.ids.len() == SCAN_CHUNK_ENTRIES {
            self.sealed.push(Arc::new(std::mem::take(&mut self.tail)));
        }
        self.tail.ids.push(entry.thought_id);
        self.tail.vectors.push(entry.vector.clone());
        self.indices.push(entry.thought_index);
        self.last_hash = Some(entry.thought_hash.clone());
    }

    /// Thought id and (shared) vector at `position`.
    pub(crate) fn entry_at(&self, position: usize) -> Option<(Uuid, VectorRow)> {
        let chunk = self
            .sealed
            .get(position / SCAN_CHUNK_ENTRIES)
            .map_or(&self.tail, |sealed| sealed.as_ref());
        let offset = position % SCAN_CHUNK_ENTRIES;
        Some((*chunk.ids.get(offset)?, chunk.vectors.get(offset)?.clone()))
    }

    /// Snapshot of the sealed chunks: `O(entries / SCAN_CHUNK_ENTRIES)`
    /// pointer clones, no vector copied.
    pub(crate) fn snapshot(&self) -> ScanSnapshot {
        ScanSnapshot {
            epoch: self.epoch,
            chunks: self.sealed.clone(),
        }
    }

    /// Scan positions `from..to` of the live store for vectors similar to
    /// `vector`, appending the candidates (score at least `threshold`, in
    /// position order) to `out`.
    pub(crate) fn scan_range(
        &self,
        from: usize,
        to: usize,
        query: &ScanQuery<'_>,
        out: &mut Vec<ImplicitNeighbor>,
    ) {
        let to = to.min(self.len());
        let mut position = from;
        while position < to {
            let chunk_number = position / SCAN_CHUNK_ENTRIES;
            let chunk = self
                .sealed
                .get(chunk_number)
                .map_or(&self.tail, |sealed| sealed.as_ref());
            let start = position % SCAN_CHUNK_ENTRIES;
            let end = (to - chunk_number * SCAN_CHUNK_ENTRIES).min(SCAN_CHUNK_ENTRIES);
            scan_chunk(chunk, start..end, query, out);
            position = (chunk_number + 1) * SCAN_CHUNK_ENTRIES;
        }
    }
}

/// Compare `vector` with positions `start..end` of one chunk.
fn scan_chunk(
    chunk: &ScanChunk,
    positions: std::ops::Range<usize>,
    query: &ScanQuery<'_>,
    out: &mut Vec<ImplicitNeighbor>,
) {
    for position in positions.start..positions.end.min(chunk.ids.len()) {
        if chunk.ids[position] == query.id {
            continue;
        }
        let Some(score) = cosine_similarity(query.vector, &chunk.vectors[position]) else {
            continue;
        };
        if score < query.threshold {
            continue;
        }
        out.push(ImplicitNeighbor {
            thought_id: chunk.ids[position],
            cosine_score: score,
        });
    }
}

/// Scan every item against the sealed chunks of `snapshot`, using up to
/// `threads` threads, and return one candidate list per item (each item's own
/// threshold applies).
///
/// Item `i` is compared with positions `0..min(prefix_len, snapshot.len())`;
/// the caller finishes the rest from the live store. Chunks are walked
/// outermost so each chunk is read once for all items. The candidates of an
/// item are in position order regardless of the thread count.
pub(crate) fn scan_snapshot(
    snapshot: &ScanSnapshot,
    items: &[ScanItem<'_>],
    threads: usize,
) -> Vec<Vec<ImplicitNeighbor>> {
    let chunk_count = snapshot.chunks.len();
    let threads = threads.clamp(1, chunk_count.max(1));
    let per_thread = chunk_count.div_ceil(threads).max(1);
    let scan_range = |first: usize, last: usize| -> Vec<Vec<ImplicitNeighbor>> {
        let mut found: Vec<Vec<ImplicitNeighbor>> = items.iter().map(|_| Vec::new()).collect();
        for (chunk_number, chunk) in snapshot.chunks[first..last].iter().enumerate() {
            let base = (first + chunk_number) * SCAN_CHUNK_ENTRIES;
            for (item, out) in items.iter().zip(&mut found) {
                if item.prefix_len > base {
                    let end = (item.prefix_len - base).min(SCAN_CHUNK_ENTRIES);
                    scan_chunk(chunk, 0..end, &item.query, out);
                }
            }
        }
        found
    };
    if threads == 1 {
        return scan_range(0, chunk_count);
    }
    let parts: Vec<Vec<Vec<ImplicitNeighbor>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|thread| {
                let first = (thread * per_thread).min(chunk_count);
                let last = ((thread + 1) * per_thread).min(chunk_count);
                let scan_range = &scan_range;
                scope.spawn(move || scan_range(first, last))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
            })
            .collect()
    });
    let mut merged: Vec<Vec<ImplicitNeighbor>> = items.iter().map(|_| Vec::new()).collect();
    for part in parts {
        for (out, found) in merged.iter_mut().zip(part) {
            out.extend(found);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::EmbeddingMetadata;

    fn sidecar(count: usize) -> VectorSidecar {
        let entries = (0..count)
            .map(|i| {
                // Few distinct directions, so many pairs clear the threshold.
                let vector: Vec<f32> = (0..4)
                    .map(|d| (((i % 5) * 7 + d * 3) % 11) as f32 + 1.0)
                    .collect();
                VectorSidecarEntry::new(
                    Uuid::from_u128(i as u128 + 1),
                    i as u64,
                    format!("h{i}"),
                    vector,
                )
            })
            .collect();
        VectorSidecar::build(
            "scan",
            EmbeddingMetadata::new("scan-model", 4, "v1"),
            count,
            count.checked_sub(1).map(|last| format!("h{last}")),
            chrono::Utc::now(),
            entries,
        )
        .unwrap()
    }

    /// Every candidate a plain scan over the sidecar would produce, in order.
    fn brute_force(
        sidecar: &VectorSidecar,
        position: usize,
        threshold: f32,
    ) -> Vec<ImplicitNeighbor> {
        let me = &sidecar.entries[position];
        sidecar.entries[..position]
            .iter()
            .filter(|other| other.thought_id != me.thought_id)
            .filter_map(|other| {
                let score = cosine_similarity(&me.vector, &other.vector)?;
                (score >= threshold).then_some(ImplicitNeighbor {
                    thought_id: other.thought_id,
                    cosine_score: score,
                })
            })
            .collect()
    }

    /// A snapshot scan plus the live-store remainder equals one scan over the
    /// whole sidecar, for any thread count and across chunk boundaries.
    #[test]
    fn test_snapshot_plus_delta_equals_a_full_scan() {
        let sidecar = sidecar(3 * SCAN_CHUNK_ENTRIES + 41);
        let mut store = VectorScanStore::new(4);
        store.sync_with(&sidecar);
        assert_eq!(store.len(), sidecar.entries.len());
        let snapshot = store.snapshot();
        assert_eq!(snapshot.len(), 3 * SCAN_CHUNK_ENTRIES);
        let positions = [
            0,
            1,
            SCAN_CHUNK_ENTRIES,
            2 * SCAN_CHUNK_ENTRIES + 5,
            3 * SCAN_CHUNK_ENTRIES + 40,
        ];
        let threshold = 0.9;
        for threads in [1, 2, 3, 8] {
            let items: Vec<ScanItem<'_>> = positions
                .iter()
                .map(|&position| ScanItem {
                    query: ScanQuery {
                        id: sidecar.entries[position].thought_id,
                        vector: &sidecar.entries[position].vector,
                        threshold,
                    },
                    prefix_len: store.position_before(position as u64),
                })
                .collect();
            let mut found = scan_snapshot(&snapshot, &items, threads);
            for ((&position, item), found) in positions.iter().zip(&items).zip(&mut found) {
                store.scan_range(
                    snapshot.len().min(item.prefix_len),
                    item.prefix_len,
                    &item.query,
                    found,
                );
                let expected = brute_force(&sidecar, position, threshold);
                assert_eq!(*found, expected, "position {position}, {threads} threads");
            }
        }
    }

    /// Growing the sidecar extends the store in place; replacing it starts a
    /// new store with a new identity, which is how a stale scan is recognised.
    #[test]
    fn test_sync_extends_in_place_and_restarts_on_replacement() {
        let mut store = VectorScanStore::new(4);
        store.sync_with(&sidecar(10));
        let epoch = store.epoch();
        store.sync_with(&sidecar(25));
        assert_eq!((store.len(), store.epoch()), (25, epoch));
        let mut replaced = sidecar(25);
        replaced.entries[24].thought_hash = "different".to_owned();
        store.sync_with(&replaced);
        assert_ne!(store.epoch(), epoch);
        assert_eq!(store.len(), 25);
        let mut shorter = sidecar(7);
        shorter.entries[6].thought_hash = "other".to_owned();
        let epoch = store.epoch();
        store.sync_with(&shorter);
        assert_ne!(store.epoch(), epoch);
        assert_eq!(store.len(), 7);
    }

    /// The store shares the sidecar's rows instead of copying them: every
    /// stored vector is the same row of the same chunk, at the same address, as
    /// the sidecar's, and a snapshot and a scan add no copy.
    #[test]
    fn test_store_shares_the_sidecar_vectors_instead_of_copying_them() {
        let sidecar = sidecar(2 * SCAN_CHUNK_ENTRIES + 9);
        let mut store = VectorScanStore::new(4);
        store.sync_with(&sidecar);
        let snapshot = store.snapshot();
        let items = [ScanItem {
            query: ScanQuery {
                id: sidecar.entries[0].thought_id,
                vector: &sidecar.entries[0].vector,
                threshold: 0.5,
            },
            prefix_len: store.len(),
        }];
        let _ = scan_snapshot(&snapshot, &items, 2);
        for (position, entry) in sidecar.entries.iter().enumerate() {
            let (_, shared) = store.entry_at(position).expect("stored");
            assert!(
                shared.same_row(&entry.vector),
                "position {position} was copied"
            );
            assert_eq!(shared.lines().as_ptr(), entry.vector.lines().as_ptr());
        }
    }
}
