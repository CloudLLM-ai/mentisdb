use std::collections::{BTreeMap, BTreeSet};

/// Configuration for deterministic summary candidate selection.
///
/// This type only controls source selection. It does not create, append, or
/// persist summary thoughts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryBuildConfig {
    /// Maximum number of source thoughts in one candidate window.
    pub window_size: usize,
    /// Number of source thoughts shared between adjacent windows in a group.
    pub overlap: usize,
    /// Keep thoughts from different sessions in separate candidate streams.
    pub by_session: bool,
    /// Keep thoughts from different agents in separate candidate streams.
    pub by_agent: bool,
    /// Keep thoughts from different entity types in separate candidate streams.
    pub by_entity_type: bool,
}

impl Default for SummaryBuildConfig {
    fn default() -> Self {
        Self {
            window_size: 50,
            overlap: 0,
            by_session: true,
            by_agent: false,
            by_entity_type: true,
        }
    }
}

/// Minimal read-only thought data needed to select summary candidates.
///
/// Callers can adapt full `Thought` records into this shape without coupling the
/// candidate selector to chain storage, LLM generation, or persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummarySourceThought<'a> {
    /// Append-order index of the source thought in its chain.
    pub index: u64,
    /// Stable source thought id.
    pub thought_id: &'a str,
    /// Optional session identifier associated with the source thought.
    pub session_id: Option<&'a str>,
    /// Stable producing agent id.
    pub agent_id: &'a str,
    /// Optional entity type label associated with the source thought.
    pub entity_type: Option<&'a str>,
}

/// Grouping metadata shared by every source thought in a candidate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SummaryGroup {
    /// Session id when [`SummaryBuildConfig::by_session`] is enabled.
    pub session_id: Option<String>,
    /// Agent id when [`SummaryBuildConfig::by_agent`] is enabled.
    pub agent_id: Option<String>,
    /// Entity type when [`SummaryBuildConfig::by_entity_type`] is enabled.
    pub entity_type: Option<String>,
}

/// Existing `Summarizes` coverage used to skip already summarized ranges.
///
/// Each value represents the source thoughts covered by one or more already
/// appended summary thoughts. Candidate windows whose every source is covered
/// by these ids or indices are omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryCoverage {
    /// Append-order indices already targeted by `Summarizes` relations.
    pub summarized_indices: Vec<u64>,
    /// Stable ids already targeted by `Summarizes` relations.
    pub summarized_ids: Vec<String>,
}

/// Deterministic source set that a caller may summarize and append later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryCandidate {
    /// Append-order indices included in this candidate window.
    pub source_indices: Vec<u64>,
    /// Stable source thought ids included in this candidate window.
    pub source_ids: Vec<String>,
    /// Shared grouping metadata for this candidate window.
    pub group: SummaryGroup,
    /// First source thought index in this candidate window.
    pub start_index: u64,
    /// Last source thought index in this candidate window.
    pub end_index: u64,
}

/// Build deterministic summary source candidates from committed thought-like inputs.
///
/// The selector sorts inputs by append index, partitions them according to the
/// requested grouping fields, emits fixed-size windows with optional overlap,
/// and skips windows already covered by existing `Summarizes` relations.
pub fn build_summary_candidates(
    thoughts: &[SummarySourceThought<'_>],
    config: SummaryBuildConfig,
    existing_coverage: &[SummaryCoverage],
) -> Vec<SummaryCandidate> {
    if thoughts.is_empty() || config.window_size == 0 {
        return Vec::new();
    }

    let covered = CoveredSources::from_coverage(existing_coverage);
    let mut grouped: BTreeMap<SummaryGroup, Vec<SummarySourceThought<'_>>> = BTreeMap::new();

    for thought in thoughts.iter().copied() {
        grouped
            .entry(group_for(thought, config))
            .or_default()
            .push(thought);
    }

    let step = config.window_size.saturating_sub(config.overlap).max(1);
    let mut candidates = Vec::new();

    for (group, mut group_thoughts) in grouped {
        group_thoughts.sort_by(|left, right| {
            left.index
                .cmp(&right.index)
                .then_with(|| left.thought_id.cmp(right.thought_id))
        });

        let mut start = 0;
        while start < group_thoughts.len() {
            let end = (start + config.window_size).min(group_thoughts.len());
            let window = &group_thoughts[start..end];

            if !covered.covers_all(window) {
                candidates.push(candidate_from_window(window, group.clone()));
            }

            if end == group_thoughts.len() {
                break;
            }
            start += step;
        }
    }

    candidates.sort_by(|left, right| {
        left.start_index
            .cmp(&right.start_index)
            .then_with(|| left.end_index.cmp(&right.end_index))
            .then_with(|| left.group.cmp(&right.group))
            .then_with(|| left.source_ids.cmp(&right.source_ids))
    });

    candidates
}

fn group_for(thought: SummarySourceThought<'_>, config: SummaryBuildConfig) -> SummaryGroup {
    SummaryGroup {
        session_id: config
            .by_session
            .then(|| thought.session_id.map(str::to_owned))
            .flatten(),
        agent_id: config.by_agent.then(|| thought.agent_id.to_owned()),
        entity_type: config
            .by_entity_type
            .then(|| thought.entity_type.map(str::to_owned))
            .flatten(),
    }
}

fn candidate_from_window(
    window: &[SummarySourceThought<'_>],
    group: SummaryGroup,
) -> SummaryCandidate {
    let source_indices = window
        .iter()
        .map(|thought| thought.index)
        .collect::<Vec<_>>();
    let source_ids = window
        .iter()
        .map(|thought| thought.thought_id.to_owned())
        .collect::<Vec<_>>();
    let start_index = source_indices[0];
    let end_index = *source_indices
        .last()
        .expect("candidate windows are non-empty");

    SummaryCandidate {
        source_indices,
        source_ids,
        group,
        start_index,
        end_index,
    }
}

#[derive(Debug, Default)]
struct CoveredSources {
    indices: BTreeSet<u64>,
    ids: BTreeSet<String>,
}

impl CoveredSources {
    fn from_coverage(coverage: &[SummaryCoverage]) -> Self {
        let mut covered = Self::default();
        for entry in coverage {
            covered
                .indices
                .extend(entry.summarized_indices.iter().copied());
            covered.ids.extend(entry.summarized_ids.iter().cloned());
        }
        covered
    }

    fn covers_all(&self, thoughts: &[SummarySourceThought<'_>]) -> bool {
        !thoughts.is_empty()
            && thoughts.iter().all(|thought| {
                self.indices.contains(&thought.index) || self.ids.contains(thought.thought_id)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(index: u64) -> SummarySourceThought<'static> {
        SummarySourceThought {
            index,
            thought_id: match index {
                0 => "thought-0",
                1 => "thought-1",
                2 => "thought-2",
                3 => "thought-3",
                4 => "thought-4",
                5 => "thought-5",
                _ => "thought-n",
            },
            session_id: Some("session-a"),
            agent_id: "agent-a",
            entity_type: Some("Memory"),
        }
    }

    #[test]
    fn builds_non_overlapping_windows_in_index_order() {
        let thoughts = vec![source(3), source(0), source(2), source(1), source(4)];
        let candidates = build_summary_candidates(
            &thoughts,
            SummaryBuildConfig {
                window_size: 2,
                overlap: 0,
                ..SummaryBuildConfig::default()
            },
            &[],
        );

        let windows = candidates
            .iter()
            .map(|candidate| candidate.source_indices.as_slice())
            .collect::<Vec<_>>();
        assert_eq!(windows, vec![&[0, 1][..], &[2, 3][..], &[4][..]]);
    }

    #[test]
    fn builds_overlapping_windows() {
        let thoughts = (0..5).map(source).collect::<Vec<_>>();
        let candidates = build_summary_candidates(
            &thoughts,
            SummaryBuildConfig {
                window_size: 3,
                overlap: 1,
                ..SummaryBuildConfig::default()
            },
            &[],
        );

        let windows = candidates
            .iter()
            .map(|candidate| candidate.source_indices.as_slice())
            .collect::<Vec<_>>();
        assert_eq!(windows, vec![&[0, 1, 2][..], &[2, 3, 4][..]]);
    }

    #[test]
    fn partitions_by_session_agent_and_entity_when_configured() {
        let thoughts = vec![
            SummarySourceThought {
                index: 0,
                thought_id: "a0",
                session_id: Some("session-a"),
                agent_id: "agent-a",
                entity_type: Some("Task"),
            },
            SummarySourceThought {
                index: 1,
                thought_id: "a1",
                session_id: Some("session-a"),
                agent_id: "agent-a",
                entity_type: Some("Task"),
            },
            SummarySourceThought {
                index: 2,
                thought_id: "b0",
                session_id: Some("session-b"),
                agent_id: "agent-a",
                entity_type: Some("Task"),
            },
            SummarySourceThought {
                index: 3,
                thought_id: "c0",
                session_id: Some("session-a"),
                agent_id: "agent-b",
                entity_type: Some("Task"),
            },
            SummarySourceThought {
                index: 4,
                thought_id: "d0",
                session_id: Some("session-a"),
                agent_id: "agent-a",
                entity_type: Some("Decision"),
            },
        ];

        let candidates = build_summary_candidates(
            &thoughts,
            SummaryBuildConfig {
                window_size: 10,
                overlap: 0,
                by_session: true,
                by_agent: true,
                by_entity_type: true,
            },
            &[],
        );

        let windows = candidates
            .iter()
            .map(|candidate| candidate.source_indices.as_slice())
            .collect::<Vec<_>>();
        assert_eq!(windows, vec![&[0, 1][..], &[2][..], &[3][..], &[4][..]]);
        assert_eq!(
            candidates[0].group,
            SummaryGroup {
                session_id: Some("session-a".to_string()),
                agent_id: Some("agent-a".to_string()),
                entity_type: Some("Task".to_string()),
            }
        );
    }

    #[test]
    fn skips_windows_already_covered_by_summarizes_relations() {
        let thoughts = (0..5).map(source).collect::<Vec<_>>();
        let coverage = vec![SummaryCoverage {
            summarized_indices: vec![0, 1],
            summarized_ids: vec!["thought-4".to_string()],
        }];

        let candidates = build_summary_candidates(
            &thoughts,
            SummaryBuildConfig {
                window_size: 2,
                overlap: 0,
                ..SummaryBuildConfig::default()
            },
            &coverage,
        );

        let windows = candidates
            .iter()
            .map(|candidate| candidate.source_indices.as_slice())
            .collect::<Vec<_>>();
        assert_eq!(windows, vec![&[2, 3][..]]);
    }
}
