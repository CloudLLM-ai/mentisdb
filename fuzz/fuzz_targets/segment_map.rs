#![no_main]
//! Fuzzes the segment model: for any set of segments and governance records,
//! a public reader sees a subset of what an authorized reader sees, governance
//! records are never public, and lookups never panic.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mentisdb::audience::{Audience, Segment, SegmentKind, SegmentMap};

#[derive(Arbitrary, Debug)]
struct Input {
    segments: Vec<(u16, bool, u8)>,
    records: Vec<u16>,
    probes: Vec<u16>,
}

fuzz_target!(|input: Input| {
    let segments = input
        .segments
        .iter()
        .map(|(start, private, seq)| Segment {
            start_index: u64::from(*start),
            kind: if *private { SegmentKind::Private } else { SegmentKind::Public },
            seq: u64::from(*seq),
        })
        .collect();
    let mut map = SegmentMap::from_segments(segments);
    let mut records = input.records.clone();
    records.sort_unstable();
    for record in &records {
        map.push_record(u64::from(*record));
    }
    for probe in &input.probes {
        let index = u64::from(*probe);
        // Authorized sees everything; public sees only public positions.
        assert!(map.is_visible(index, Audience::Authorized));
        if map.is_visible(index, Audience::Public) {
            assert_eq!(map.kind_at(index), SegmentKind::Public);
            assert!(!map.records().contains(&index));
        }
        if map.records().contains(&index) {
            assert!(!map.is_visible(index, Audience::Public));
        }
    }
});
