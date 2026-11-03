"""
pymentisdb - Python client library for MentisDB.

MentisDB is an append-only semantic memory server for long-running agents.
This library provides Python bindings for the MentisDB REST API, including
a LangChain-compatible memory interface.

Example:
    >>> from pymentisdb import MentisDbClient, ThoughtType
    >>> client = MentisDbClient()
    >>> thought = client.append_thought(
    ...     thought_type=ThoughtType.INSIGHT,
    ...     content="Rate limiting is the real bottleneck."
    ... )
    >>> print(f"Appended thought {thought.id}")
"""

__version__ = "0.10.0"

from .read_auth import ReadSigner
from .client import MentisDbClient
from .types import (
    ThoughtType,
    ThoughtRole,
    MemoryScope,
    DfScope,
    DfCutoffs,
    VectorFusion,
    GraphMode,
    GraphOptions,
    ThoughtRelation,
    ThoughtRelationKind,
    ThoughtInput,
    Thought,
    AgentRecord,
    Provenance,
    RankedSearchHit,
    RankedSearchResponse,
    CompactThought,
    CompactRankedSearchHit,
    CompactRankedSearchResponse,
    SearchResponse,
    ContextBundle,
    ContextBundlesResponse,
    ChainSummary,
    ListChainsResponse,
)
from .langchain import MentisDbMemory

__all__ = [
    "ReadSigner",
    "__version__",
    "MentisDbClient",
    "MentisDbMemory",
    "ThoughtType",
    "ThoughtRole",
    "MemoryScope",
    "DfScope",
    "DfCutoffs",
    "VectorFusion",
    "GraphMode",
    "GraphOptions",
    "ThoughtRelation",
    "ThoughtRelationKind",
    "ThoughtInput",
    "Thought",
    "AgentRecord",
    "Provenance",
    "RankedSearchHit",
    "RankedSearchResponse",
    "CompactThought",
    "CompactRankedSearchHit",
    "CompactRankedSearchResponse",
    "SearchResponse",
    "ContextBundle",
    "ContextBundlesResponse",
    "ChainSummary",
    "ListChainsResponse",
]
