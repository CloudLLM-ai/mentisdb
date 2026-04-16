"""
Python dataclasses matching MentisDB types.
"""

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any, Optional


class ThoughtType(str, Enum):
    """Semantic category describing what changed in the agent's internal model."""
    PREFERENCE_UPDATE = "PreferenceUpdate"
    USER_TRAIT = "UserTrait"
    RELATIONSHIP_UPDATE = "RelationshipUpdate"
    FINDING = "Finding"
    INSIGHT = "Insight"
    FACT_LEARNED = "FactLearned"
    PATTERN_DETECTED = "PatternDetected"
    HYPOTHESIS = "Hypothesis"
    MISTAKE = "Mistake"
    CORRECTION = "Correction"
    LESSON_LEARNED = "LessonLearned"
    ASSUMPTION_INVALIDATED = "AssumptionInvalidated"
    CONSTRAINT = "Constraint"
    PLAN = "Plan"
    SUBGOAL = "Subgoal"
    DECISION = "Decision"
    STRATEGY_SHIFT = "StrategyShift"
    WONDER = "Wonder"
    QUESTION = "Question"
    IDEA = "Idea"
    EXPERIMENT = "Experiment"
    ACTION_TAKEN = "ActionTaken"
    TASK_COMPLETE = "TaskComplete"
    CHECKPOINT = "Checkpoint"
    STATE_SNAPSHOT = "StateSnapshot"
    HANDOFF = "Handoff"
    SUMMARY = "Summary"
    SURPRISE = "Surprise"
    REFRAME = "Reframe"
    GOAL = "Goal"
    LLM_EXTRACTED = "LLMExtracted"


class ThoughtRole(str, Enum):
    """Operational role of a thought inside the system."""
    MEMORY = "Memory"
    WORKING_MEMORY = "WorkingMemory"
    SUMMARY = "Summary"
    COMPRESSION = "Compression"
    CHECKPOINT = "Checkpoint"
    HANDOFF = "Handoff"
    AUDIT = "Audit"
    RETROSPECTIVE = "Retrospective"


class MemoryScope(str, Enum):
    """Visibility scope for a thought."""
    USER = "User"
    SESSION = "Session"
    AGENT = "Agent"

    def as_tag(self) -> str:
        return f"scope:{self.value.lower()}"

    def as_api_value(self) -> str:
        return self.value.lower()


class ThoughtRelationKind(str, Enum):
    """Why a thought points to another thought."""
    REFERENCES = "References"
    SUMMARIZES = "Summarizes"
    CORRECTS = "Corrects"
    INVALIDATES = "Invalidates"
    CAUSED_BY = "CausedBy"
    SUPPORTS = "Supports"
    CONTRADICTS = "Contradicts"
    DERIVED_FROM = "DerivedFrom"
    CONTINUES_FROM = "ContinuesFrom"
    BRANCHES_FROM = "BranchesFrom"
    RELATED_TO = "RelatedTo"
    SUPERSEDES = "Supersedes"


@dataclass
class ThoughtRelation:
    """Typed edge in the thought graph."""
    kind: ThoughtRelationKind
    target_id: str
    chain_key: Optional[str] = None
    valid_at: Optional[datetime] = None
    invalid_at: Optional[datetime] = None

    def to_dict(self) -> dict:
        result = {
            "kind": self.kind.value,
            "target_id": self.target_id,
        }
        if self.chain_key:
            result["chain_key"] = self.chain_key
        if self.valid_at:
            result["valid_at"] = self.valid_at.isoformat()
        if self.invalid_at:
            result["invalid_at"] = self.invalid_at.isoformat()
        return result


@dataclass
class ThoughtInput:
    """Builder-like input struct used to append rich thoughts."""
    thought_type: ThoughtType
    content: str
    agent_id: Optional[str] = None
    session_id: Optional[str] = None
    agent_name: Optional[str] = None
    agent_owner: Optional[str] = None
    signing_key_id: Optional[str] = None
    thought_signature: Optional[bytes] = None
    role: ThoughtRole = ThoughtRole.MEMORY
    confidence: Optional[float] = None
    importance: float = 0.5
    tags: list[str] = field(default_factory=list)
    concepts: list[str] = field(default_factory=list)
    refs: list[int] = field(default_factory=list)
    relations: list[ThoughtRelation] = field(default_factory=list)
    entity_type: Optional[str] = None
    source_episode: Optional[str] = None
    scope: Optional[MemoryScope] = None

    def to_dict(self) -> dict:
        result = {
            "thought_type": self.thought_type.value,
            "content": self.content,
            "role": self.role.value,
            "importance": self.importance,
            "tags": self.tags,
            "concepts": self.concepts,
            "refs": self.refs,
        }
        if self.agent_id:
            result["agent_id"] = self.agent_id
        if self.session_id:
            result["session_id"] = self.session_id
        if self.agent_name:
            result["agent_name"] = self.agent_name
        if self.agent_owner:
            result["agent_owner"] = self.agent_owner
        if self.signing_key_id:
            result["signing_key_id"] = self.signing_key_id
        if self.thought_signature:
            result["thought_signature"] = list(self.thought_signature)
        if self.confidence is not None:
            result["confidence"] = self.confidence
        if self.relations:
            result["relations"] = [r.to_dict() for r in self.relations]
        if self.entity_type:
            result["entity_type"] = self.entity_type
        if self.source_episode:
            result["source_episode"] = self.source_episode
        if self.scope:
            result["scope"] = self.scope.as_api_value()
        return result


@dataclass
class Thought:
    """A single durable thought record."""
    id: str
    index: int
    timestamp: datetime
    thought_type: ThoughtType
    content: str
    agent_id: str
    prev_hash: str
    hash: str
    role: ThoughtRole = ThoughtRole.MEMORY
    session_id: Optional[str] = None
    signing_key_id: Optional[str] = None
    thought_signature: Optional[bytes] = None
    confidence: Optional[float] = None
    importance: float = 0.5
    tags: list[str] = field(default_factory=list)
    concepts: list[str] = field(default_factory=list)
    refs: list[int] = field(default_factory=list)
    relations: list[ThoughtRelation] = field(default_factory=list)
    entity_type: Optional[str] = None
    source_episode: Optional[str] = None

    @classmethod
    def from_dict(cls, data: dict) -> "Thought":
        return cls(
            id=data["id"],
            index=data["index"],
            timestamp=datetime.fromisoformat(data["timestamp"]),
            thought_type=ThoughtType(data["thought_type"]),
            content=data["content"],
            agent_id=data["agent_id"],
            prev_hash=data["prev_hash"],
            hash=data["hash"],
            role=ThoughtRole(data.get("role", "Memory")),
            session_id=data.get("session_id"),
            signing_key_id=data.get("signing_key_id"),
            thought_signature=bytes(data["thought_signature"]) if data.get("thought_signature") else None,
            confidence=data.get("confidence"),
            importance=data.get("importance", 0.5),
            tags=data.get("tags", []),
            concepts=data.get("concepts", []),
            refs=data.get("refs", []),
            relations=[ThoughtRelation(
                kind=ThoughtRelationKind(r["kind"]),
                target_id=r["target_id"],
                chain_key=r.get("chain_key"),
            ) for r in data.get("relations", [])],
            entity_type=data.get("entity_type"),
            source_episode=data.get("source_episode"),
        )


@dataclass
class AgentRecord:
    """Registry entry describing one durable agent identity."""
    agent_id: str
    display_name: str
    status: str
    owner: Optional[str] = None
    description: Optional[str] = None
    aliases: list[str] = field(default_factory=list)
    public_keys: list[Any] = field(default_factory=list)
    first_seen_index: Optional[int] = None
    last_seen_index: Optional[int] = None
    first_seen_at: Optional[datetime] = None
    last_seen_at: Optional[datetime] = None
    thought_count: int = 0

    @classmethod
    def from_dict(cls, data: dict) -> "AgentRecord":
        return cls(
            agent_id=data["agent_id"],
            display_name=data["display_name"],
            status=data.get("status", "Active"),
            owner=data.get("owner"),
            description=data.get("description"),
            aliases=data.get("aliases", []),
            public_keys=data.get("public_keys", []),
            first_seen_index=data.get("first_seen_index"),
            last_seen_index=data.get("last_seen_index"),
            first_seen_at=datetime.fromisoformat(data["first_seen_at"]) if data.get("first_seen_at") else None,
            last_seen_at=datetime.fromisoformat(data["last_seen_at"]) if data.get("last_seen_at") else None,
            thought_count=data.get("thought_count", 0),
        )


@dataclass
class RankedSearchScore:
    """Score breakdown for a ranked search hit."""
    lexical: float
    vector: float
    graph: float
    relation: float
    seed_support: float
    importance: float
    confidence: float
    recency: float
    session_cohesion: float
    rrf: float
    total: float


@dataclass
class RankedSearchHit:
    """A single ranked search result."""
    chain_key: str
    thought: Thought
    score: RankedSearchScore
    matched_terms: list[str]
    match_sources: list[str]
    graph_distance: Optional[int] = None
    graph_seed_paths: int = 0
    graph_relation_kinds: list[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, data: dict) -> "RankedSearchHit":
        score_data = data["score"]
        score = RankedSearchScore(
            lexical=score_data["lexical"],
            vector=score_data["vector"],
            graph=score_data["graph"],
            relation=score_data["relation"],
            seed_support=score_data["seed_support"],
            importance=score_data["importance"],
            confidence=score_data["confidence"],
            recency=score_data["recency"],
            session_cohesion=score_data["session_cohesion"],
            rrf=score_data["rrf"],
            total=score_data["total"],
        )
        thought_data = data["thought"]
        thought = Thought.from_dict(thought_data) if isinstance(thought_data, dict) else thought_data
        return cls(
            chain_key=data["chain_key"],
            thought=thought,
            score=score,
            matched_terms=data.get("matched_terms", []),
            match_sources=data.get("match_sources", []),
            graph_distance=data.get("graph_distance"),
            graph_seed_paths=data.get("graph_seed_paths", 0),
            graph_relation_kinds=data.get("graph_relation_kinds", []),
        )


@dataclass
class RankedSearchResponse:
    """Response from a ranked search query."""
    backend: str
    total: int
    results: list[RankedSearchHit]


@dataclass
class ContextBundleSeed:
    """Seed thought for a context bundle."""
    chain_key: Optional[str]
    thought_id: str
    thought_index: Optional[int]
    lexical_score: float
    matched_terms: list[str]
    thought: Optional[Thought] = None

    @classmethod
    def from_dict(cls, data: dict) -> "ContextBundleSeed":
        thought = None
        if data.get("thought"):
            thought = Thought.from_dict(data["thought"]) if isinstance(data["thought"], dict) else data["thought"]
        return cls(
            chain_key=data.get("locator", {}).get("chain_key"),
            thought_id=data.get("locator", {}).get("thought_id", ""),
            thought_index=data.get("locator", {}).get("thought_index"),
            lexical_score=data.get("lexical_score", 0.0),
            matched_terms=data.get("matched_terms", []),
            thought=thought,
        )


@dataclass
class ContextBundleHit:
    """A supporting thought in a context bundle."""
    chain_key: Optional[str]
    thought_id: str
    thought_index: Optional[int]
    thought: Optional[Thought]
    depth: int
    seed_path_count: int
    relation_kinds: list[str]

    @classmethod
    def from_dict(cls, data: dict) -> "ContextBundleHit":
        thought = None
        if data.get("thought"):
            thought = Thought.from_dict(data["thought"]) if isinstance(data["thought"], dict) else data["thought"]
        return cls(
            chain_key=data.get("locator", {}).get("chain_key"),
            thought_id=data.get("locator", {}).get("thought_id", ""),
            thought_index=data.get("locator", {}).get("thought_index"),
            thought=thought,
            depth=data.get("depth", 0),
            seed_path_count=data.get("seed_path_count", 0),
            relation_kinds=data.get("relation_kinds", []),
        )


@dataclass
class ContextBundle:
    """A seed with its supporting context."""
    seed: ContextBundleSeed
    support: list[ContextBundleHit]


@dataclass
class ContextBundlesResponse:
    """Response from a context bundles query."""
    total_bundles: int
    consumed_hits: int
    bundles: list[ContextBundle]


@dataclass
class ChainSummary:
    """Summary of a registered thought chain."""
    chain_key: str
    version: int
    storage_adapter: str
    thought_count: int
    agent_count: int
    storage_location: str

    @classmethod
    def from_dict(cls, data: dict) -> "ChainSummary":
        return cls(
            chain_key=data["chain_key"],
            version=data["version"],
            storage_adapter=data["storage_adapter"],
            thought_count=data["thought_count"],
            agent_count=data["agent_count"],
            storage_location=data["storage_location"],
        )


@dataclass
class ListChainsResponse:
    """Response from listing all chains."""
    default_chain_key: str
    chain_keys: list[str]
    chains: list[ChainSummary]
