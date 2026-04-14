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

__version__ = "0.9.0"

from .client import MentisDbClient
from .types import (
    ThoughtType,
    ThoughtRole,
    MemoryScope,
    ThoughtRelation,
    ThoughtRelationKind,
    ThoughtInput,
    Thought,
    AgentRecord,
    RankedSearchHit,
    RankedSearchResponse,
    ContextBundle,
    ContextBundlesResponse,
    ChainSummary,
    ListChainsResponse,
)
from .langchain import MentisDbMemory

__all__ = [
    "__version__",
    "MentisDbClient",
    "MentisDbMemory",
    "ThoughtType",
    "ThoughtRole",
    "MemoryScope",
    "ThoughtRelation",
    "ThoughtRelationKind",
    "ThoughtInput",
    "Thought",
    "AgentRecord",
    "RankedSearchHit",
    "RankedSearchResponse",
    "ContextBundle",
    "ContextBundlesResponse",
    "ChainSummary",
    "ListChainsResponse",
]
