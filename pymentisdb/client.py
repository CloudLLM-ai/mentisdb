"""
MentisDbClient - Python client for the MentisDB REST API.
"""

import requests
from datetime import datetime
from typing import Optional

from .types import (
    ThoughtInput,
    Thought,
    ThoughtRelation,
    ThoughtType,
    ThoughtRole,
    MemoryScope,
    RankedSearchHit,
    RankedSearchResponse,
    ContextBundle,
    ContextBundlesResponse,
    ListChainsResponse,
    ChainSummary,
    AgentRecord,
)


class MentisDbClient:
    """
    Python client for the MentisDB REST API.

    Provides a wrapper around MentisDB's HTTP REST interface for:
    - Appending thoughts to memory chains
    - Searching memory with ranked retrieval
    - Getting context bundles with supporting memories
    - Managing agent identities

    Args:
        base_url: Base URL of the MentisDB REST server.
                  Defaults to "http://127.0.0.1:9472".

    Example:
        >>> client = MentisDbClient()
        >>> client.append_thought(
        ...     thought_type=ThoughtType.INSIGHT,
        ...     content="Rate limiting is the real bottleneck."
        ... )
        >>> results = client.ranked_search(text="performance optimization")
        >>> for hit in results.results:
        ...     print(hit.thought.content)
    """

    def __init__(self, base_url: str = "http://127.0.0.1:9472"):
        self.base_url = base_url.rstrip("/")
        self._session = requests.Session()

    def _post(self, endpoint: str, data: Optional[dict] = None) -> dict:
        """Make a POST request to the REST API."""
        url = f"{self.base_url}{endpoint}"
        response = self._session.post(url, json=data or {}, timeout=30)
        response.raise_for_status()
        return response.json()

    def _get(self, endpoint: str) -> dict:
        """Make a GET request to the REST API."""
        url = f"{self.base_url}{endpoint}"
        response = self._session.get(url, timeout=30)
        response.raise_for_status()
        return response.json()

    def append_thought(
        self,
        thought_type: ThoughtType,
        content: str,
        chain_key: Optional[str] = None,
        agent_id: Optional[str] = None,
        agent_name: Optional[str] = None,
        agent_owner: Optional[str] = None,
        role: ThoughtRole = ThoughtRole.MEMORY,
        importance: float = 0.5,
        confidence: Optional[float] = None,
        tags: Optional[list[str]] = None,
        concepts: Optional[list[str]] = None,
        refs: Optional[list[int]] = None,
        relations: Optional[list[ThoughtRelation]] = None,
        entity_type: Optional[str] = None,
        source_episode: Optional[str] = None,
        scope: Optional[MemoryScope] = None,
    ) -> Thought:
        """
        Append a new thought to the memory chain.

        Args:
            thought_type: Semantic category of the thought.
            content: Primary human-readable content of the thought.
            chain_key: Optional chain identifier. Uses default if not provided.
            agent_id: Optional agent identifier.
            agent_name: Optional human-readable agent name.
            agent_owner: Optional owner or grouping label.
            role: Operational role. Defaults to Memory.
            importance: Importance score (0.0-1.0). Defaults to 0.5.
            confidence: Optional confidence score (0.0-1.0).
            tags: Optional free-form tags for retrieval.
            concepts: Optional concept labels for semantic anchoring.
            refs: Optional back-references to prior thought indices.
            relations: Optional typed graph relations to prior thoughts.
            entity_type: Optional entity type label.
            source_episode: Optional episode or conversation context ID.
            scope: Optional visibility scope (User, Session, Agent).

        Returns:
            The committed Thought record.
        """
        input_data = ThoughtInput(
            thought_type=thought_type,
            content=content,
            agent_id=agent_id,
            agent_name=agent_name,
            agent_owner=agent_owner,
            role=role,
            importance=importance,
            confidence=confidence,
            tags=tags or [],
            concepts=concepts or [],
            refs=refs or [],
            relations=relations or [],
            entity_type=entity_type,
            source_episode=source_episode,
            scope=scope,
        )

        payload = input_data.to_dict()
        if agent_id:
            payload["agent_id"] = agent_id

        request_data = {
            "chain_key": chain_key,
            "thought_type": thought_type.value,
            "content": content,
            "role": role.value,
            "importance": importance,
            "tags": tags or [],
            "concepts": concepts or [],
            "refs": refs or [],
        }
        if agent_id:
            request_data["agent_id"] = agent_id
        if agent_name:
            request_data["agent_name"] = agent_name
        if agent_owner:
            request_data["agent_owner"] = agent_owner
        if confidence is not None:
            request_data["confidence"] = confidence
        if relations:
            request_data["relations"] = [r.to_dict() for r in relations]
        if entity_type:
            request_data["entity_type"] = entity_type
        if source_episode:
            request_data["source_episode"] = source_episode
        if scope:
            request_data["scope"] = scope.as_tag()

        result = self._post("/v1/thoughts", request_data)
        return Thought.from_dict(result["thought"])

    def ranked_search(
        self,
        text: Optional[str] = None,
        chain_key: Optional[str] = None,
        limit: Optional[int] = None,
        offset: Optional[int] = None,
        thought_types: Optional[list[ThoughtType]] = None,
        roles: Optional[list[ThoughtRole]] = None,
        tags_any: Optional[list[str]] = None,
        concepts_any: Optional[list[str]] = None,
        agent_ids: Optional[list[str]] = None,
        agent_names: Optional[list[str]] = None,
        agent_owners: Optional[list[str]] = None,
        min_importance: Optional[float] = None,
        min_confidence: Optional[float] = None,
        since: Optional[datetime] = None,
        until: Optional[datetime] = None,
        scope: Optional[str] = None,
        enable_reranking: Optional[bool] = None,
        rerank_k: Optional[int] = None,
        entity_type: Optional[str] = None,
    ) -> RankedSearchResponse:
        """
        Perform a ranked semantic search over thought memories.

        Args:
            text: Query text to search for.
            chain_key: Optional chain to search. Uses default if not provided.
            limit: Maximum number of results to return.
            offset: Number of results to skip (for pagination).
            thought_types: Filter by thought type categories.
            roles: Filter by operational roles.
            tags_any: Match thoughts with any of these tags.
            concepts_any: Match thoughts with any of these concepts.
            agent_ids: Filter by agent identifiers.
            agent_names: Filter by agent names.
            agent_owners: Filter by agent owners.
            min_importance: Minimum importance threshold.
            min_confidence: Minimum confidence threshold.
            since: Match thoughts after this timestamp.
            until: Match thoughts before this timestamp.
            scope: Filter by visibility scope.
            enable_reranking: Enable result reranking.
            rerank_k: Number of results to rerank.
            entity_type: Filter by entity type.

        Returns:
            RankedSearchResponse with ranked results.
        """
        request_data: dict = {"chain_key": chain_key}
        if text is not None:
            request_data["text"] = text
        if limit is not None:
            request_data["limit"] = limit
        if offset is not None:
            request_data["offset"] = offset
        if thought_types:
            request_data["thought_types"] = [t.value for t in thought_types]
        if roles:
            request_data["roles"] = [r.value for r in roles]
        if tags_any:
            request_data["tags_any"] = tags_any
        if concepts_any:
            request_data["concepts_any"] = concepts_any
        if agent_ids:
            request_data["agent_ids"] = agent_ids
        if agent_names:
            request_data["agent_names"] = agent_names
        if agent_owners:
            request_data["agent_owners"] = agent_owners
        if min_importance is not None:
            request_data["min_importance"] = min_importance
        if min_confidence is not None:
            request_data["min_confidence"] = min_confidence
        if since is not None:
            request_data["since"] = since.isoformat()
        if until is not None:
            request_data["until"] = until.isoformat()
        if scope:
            request_data["scope"] = scope
        if enable_reranking is not None:
            request_data["enable_reranking"] = enable_reranking
        if rerank_k is not None:
            request_data["rerank_k"] = rerank_k
        if entity_type:
            request_data["entity_type"] = entity_type

        result = self._post("/v1/ranked-search", request_data)
        return RankedSearchResponse(
            backend=result["backend"],
            total=result["total"],
            results=[RankedSearchHit.from_dict(h) for h in result["results"]],
        )

    def context_bundles(
        self,
        text: Optional[str] = None,
        chain_key: Optional[str] = None,
        limit: Optional[int] = None,
        offset: Optional[int] = None,
        thought_types: Optional[list[ThoughtType]] = None,
        roles: Optional[list[ThoughtRole]] = None,
        tags_any: Optional[list[str]] = None,
        concepts_any: Optional[list[str]] = None,
        agent_ids: Optional[list[str]] = None,
        agent_names: Optional[list[str]] = None,
        agent_owners: Optional[list[str]] = None,
        min_importance: Optional[float] = None,
        min_confidence: Optional[float] = None,
        since: Optional[datetime] = None,
        until: Optional[datetime] = None,
        scope: Optional[str] = None,
        enable_reranking: Optional[bool] = None,
        rerank_k: Optional[int] = None,
        entity_type: Optional[str] = None,
    ) -> ContextBundlesResponse:
        """
        Get context bundles with seed-anchored supporting context.

        Similar to ranked_search but groups results into bundles anchored
        on lexical seed matches with supporting graph-traversal context.

        Args:
            text: Query text to search for.
            chain_key: Optional chain to search.
            limit: Maximum number of bundles to return.
            offset: Number of bundles to skip.
            thought_types: Filter by thought types.
            roles: Filter by operational roles.
            tags_any: Match thoughts with any of these tags.
            concepts_any: Match thoughts with any of these concepts.
            agent_ids: Filter by agent identifiers.
            agent_names: Filter by agent names.
            agent_owners: Filter by agent owners.
            min_importance: Minimum importance threshold.
            min_confidence: Minimum confidence threshold.
            since: Match thoughts after this timestamp.
            until: Match thoughts before this timestamp.
            scope: Filter by visibility scope.
            enable_reranking: Enable result reranking.
            rerank_k: Number of results to rerank.
            entity_type: Filter by entity type.

        Returns:
            ContextBundlesResponse with seed bundles and supporting memories.
        """
        request_data: dict = {"chain_key": chain_key}
        if text is not None:
            request_data["text"] = text
        if limit is not None:
            request_data["limit"] = limit
        if offset is not None:
            request_data["offset"] = offset
        if thought_types:
            request_data["thought_types"] = [t.value for t in thought_types]
        if roles:
            request_data["roles"] = [r.value for r in roles]
        if tags_any:
            request_data["tags_any"] = tags_any
        if concepts_any:
            request_data["concepts_any"] = concepts_any
        if agent_ids:
            request_data["agent_ids"] = agent_ids
        if agent_names:
            request_data["agent_names"] = agent_names
        if agent_owners:
            request_data["agent_owners"] = agent_owners
        if min_importance is not None:
            request_data["min_importance"] = min_importance
        if min_confidence is not None:
            request_data["min_confidence"] = min_confidence
        if since is not None:
            request_data["since"] = since.isoformat()
        if until is not None:
            request_data["until"] = until.isoformat()
        if scope:
            request_data["scope"] = scope
        if enable_reranking is not None:
            request_data["enable_reranking"] = enable_reranking
        if rerank_k is not None:
            request_data["rerank_k"] = rerank_k
        if entity_type:
            request_data["entity_type"] = entity_type

        result = self._post("/v1/context-bundles", request_data)
        bundles = []
        for bundle_data in result.get("bundles", []):
            seed_data = bundle_data["seed"]
            support_data = bundle_data.get("support", [])
            seed = {
                "locator": seed_data.get("locator", {}),
                "lexical_score": seed_data.get("lexical_score", 0.0),
                "matched_terms": seed_data.get("matched_terms", []),
                "thought": seed_data.get("thought"),
            }
            support = []
            for hit_data in support_data:
                support.append({
                    "locator": hit_data.get("locator", {}),
                    "thought": hit_data.get("thought"),
                    "depth": hit_data.get("depth", 0),
                    "seed_path_count": hit_data.get("seed_path_count", 0),
                    "relation_kinds": hit_data.get("relation_kinds", []),
                    "path": hit_data.get("path"),
                })
            bundles.append(ContextBundle(
                seed=ContextBundle(seed=seed, support=support),
                support=[],
            ))
        return ContextBundlesResponse(
            total_bundles=result["total_bundles"],
            consumed_hits=result["consumed_hits"],
            bundles=bundles,
        )

    def list_chains(self) -> ListChainsResponse:
        """
        List all registered thought chains.

        Returns:
            ListChainsResponse with chain summaries.
        """
        result = self._get("/v1/chains")
        return ListChainsResponse(
            default_chain_key=result["default_chain_key"],
            chain_keys=result["chain_keys"],
            chains=[ChainSummary.from_dict(c) for c in result.get("chains", [])],
        )

    def upsert_agent(
        self,
        agent_id: str,
        chain_key: Optional[str] = None,
        display_name: Optional[str] = None,
        agent_owner: Optional[str] = None,
        description: Optional[str] = None,
        status: Optional[str] = None,
    ) -> AgentRecord:
        """
        Create or update an agent identity record.

        Args:
            agent_id: Stable agent identifier.
            chain_key: Optional chain for the agent.
            display_name: Optional friendly display name.
            agent_owner: Optional owner or grouping label.
            description: Optional description of the agent.
            status: Optional status ("active" or "revoked").

        Returns:
            The AgentRecord that was created or updated.
        """
        request_data: dict = {"agent_id": agent_id}
        if chain_key:
            request_data["chain_key"] = chain_key
        if display_name:
            request_data["display_name"] = display_name
        if agent_owner:
            request_data["agent_owner"] = agent_owner
        if description:
            request_data["description"] = description
        if status:
            request_data["status"] = status

        result = self._post("/v1/agents/upsert", request_data)
        return AgentRecord.from_dict(result["agent"])
