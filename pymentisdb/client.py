"""
MentisDbClient - Python client for the MentisDB REST API.
"""

import json
import requests
from datetime import datetime
from typing import Optional
from urllib.parse import urlsplit

from .read_auth import CONTENT_DIGEST_HEADER, READ_AUTH_HEADER, ReadSigner
from .types import (
    ThoughtInput,
    Thought,
    ThoughtRelation,
    ThoughtType,
    ThoughtRole,
    MemoryScope,
    DfScope,
    DfCutoffs,
    VectorFusion,
    GraphOptions,
    RankedSearchHit,
    RankedSearchResponse,
    CompactRankedSearchResponse,
    SearchResponse,
    ContextBundle,
    ContextBundleSeed,
    ContextBundleHit,
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
    - Searching memory with ranked retrieval, one chain or several, in the
      full or the smaller compact form
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

    def __init__(
        self,
        base_url: str = "http://127.0.0.1:9472",
        read_signer: Optional[ReadSigner] = None,
    ):
        """Create a client.

        ``read_signer`` proves possession of a key allowed on a private chain:
        every request is signed locally and the key never leaves the process.
        When it is omitted, ``MENTISDB_READ_KEY_FILE``, ``MENTISDB_READ_AGENT_ID``
        and ``MENTISDB_READ_KEY_ID`` are used if set. Without a signer the client
        reads public segments only.
        """
        self.base_url = base_url.rstrip("/")
        self._session = requests.Session()
        self._read_signer = read_signer if read_signer is not None else ReadSigner.from_env()

    def _proof_headers(self, method: str, url: str, body: bytes) -> dict:
        if self._read_signer is None:
            return {}
        parts = urlsplit(url)
        target = parts.path + (f"?{parts.query}" if parts.query else "")
        return {
            READ_AUTH_HEADER: self._read_signer.header(method, target, body),
            # Declares the digest the proof signs, so a current server accepts a
            # body over 1 MiB without buffering it; an older one ignores it.
            CONTENT_DIGEST_HEADER: self._read_signer.content_digest(body),
        }

    def _post(self, endpoint: str, data: Optional[dict] = None) -> dict:
        """Make a POST request to the REST API."""
        url = f"{self.base_url}{endpoint}"
        # The proof covers the body bytes, so serialize once and send exactly that.
        body = json.dumps(data or {}, separators=(",", ":")).encode("utf-8")
        headers = {"Content-Type": "application/json", **self._proof_headers("POST", url, body)}
        response = self._session.post(url, data=body, headers=headers, timeout=30)
        response.raise_for_status()
        return response.json()

    def _get(self, endpoint: str) -> dict:
        """Make a GET request to the REST API."""
        url = f"{self.base_url}{endpoint}"
        response = self._session.get(
            url, headers=self._proof_headers("GET", url, b""), timeout=30
        )
        response.raise_for_status()
        return response.json()

    def _serialize_scope(self, scope: Optional[MemoryScope | str]) -> Optional[str]:
        if scope is None:
            return None
        if isinstance(scope, MemoryScope):
            return scope.as_api_value()
        return scope

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
        if chain_key is not None:
            payload["chain_key"] = chain_key

        result = self._post("/v1/thoughts", payload)
        return Thought.from_dict(result["thought"])

    def _filter_payload(
        self,
        *,
        text: Optional[str],
        limit: Optional[int],
        offset: Optional[int],
        thought_types: Optional[list[ThoughtType]],
        roles: Optional[list[ThoughtRole]],
        tags_any: Optional[list[str]],
        concepts_any: Optional[list[str]],
        agent_ids: Optional[list[str]],
        agent_names: Optional[list[str]],
        agent_owners: Optional[list[str]],
        min_importance: Optional[float],
        min_confidence: Optional[float],
        since: Optional[datetime],
        until: Optional[datetime],
        scope: Optional[MemoryScope | str],
        enable_reranking: Optional[bool],
        rerank_k: Optional[int],
        entity_type: Optional[str],
    ) -> dict:
        """Request fields shared by ranked and federated search; unset ones are omitted."""
        request_data: dict = {}
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
        serialized_scope = self._serialize_scope(scope)
        if serialized_scope is not None:
            request_data["scope"] = serialized_scope
        if enable_reranking is not None:
            request_data["enable_reranking"] = enable_reranking
        if rerank_k is not None:
            request_data["rerank_k"] = rerank_k
        if entity_type:
            request_data["entity_type"] = entity_type
        return request_data

    def _extended_payload(
        self,
        *,
        graph: Optional[GraphOptions],
        as_of: Optional[datetime],
        include_invalidated: Optional[bool],
        df_scope: Optional[DfScope | str],
        df_cutoffs: Optional[DfCutoffs],
        vector_fusion: Optional[VectorFusion | str],
        resolve_successors: Optional[bool],
        collapse_duplicates: Optional[float],
        synonym_weight: Optional[float],
    ) -> dict:
        """Graph, point-in-time and ranking-knob fields; unset ones are omitted."""
        request_data: dict = {}
        if graph is not None:
            request_data["graph"] = graph.to_dict()
        if as_of is not None:
            request_data["as_of"] = as_of.isoformat()
        if include_invalidated is not None:
            request_data["include_invalidated"] = include_invalidated
        if df_scope is not None:
            request_data["df_scope"] = DfScope(df_scope).value
        if df_cutoffs is not None:
            request_data["df_cutoffs"] = df_cutoffs.to_dict()
        if vector_fusion is not None:
            request_data["vector_fusion"] = VectorFusion(vector_fusion).value
        if resolve_successors is not None:
            request_data["resolve_successors"] = resolve_successors
        if collapse_duplicates is not None:
            request_data["collapse_duplicates"] = collapse_duplicates
        if synonym_weight is not None:
            request_data["synonym_weight"] = synonym_weight
        return request_data

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
        scope: Optional[MemoryScope | str] = None,
        enable_reranking: Optional[bool] = None,
        rerank_k: Optional[int] = None,
        entity_type: Optional[str] = None,
        compact: bool = False,
        *,
        graph: Optional[GraphOptions] = None,
        as_of: Optional[datetime] = None,
        include_invalidated: Optional[bool] = None,
        df_scope: Optional[DfScope | str] = None,
        df_cutoffs: Optional[DfCutoffs] = None,
        vector_fusion: Optional[VectorFusion | str] = None,
        resolve_successors: Optional[bool] = None,
        collapse_duplicates: Optional[float] = None,
        synonym_weight: Optional[float] = None,
    ) -> SearchResponse:
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
            compact: Ask the server for compact hits, about a quarter smaller
                (fewer tokens when the results go into an LLM prompt).
                Defaults to ``False``, which sends nothing and returns the
                full form exactly as before. See below.
            graph: ``GraphOptions`` to expand the relation graph from the
                lexical seeds; expanded hits carry ``graph_*`` fields.
            as_of: Search the chain as it was at this time.
            include_invalidated: Also return superseded, corrected and
                invalidated thoughts (history); the default hides them.
            df_scope: ``DfScope`` of the lexical stop-word gate.
            df_cutoffs: ``DfCutoffs`` overrides of the gate cutoff ratios.
            vector_fusion: ``VectorFusion`` blend of vector and lexical scores.
            resolve_successors: Replace a superseded hit by its live successor
                and promote the thought contesting a contested hit (server
                default ``True``).
            collapse_duplicates: Near-duplicate collapse threshold in (0, 1];
                ``0`` disables (server default 0.9).
            synonym_weight: Weight of automatic thesaurus expansion; ``0``
                disables it (server default 0.3).

        Returns:
            ``RankedSearchResponse`` (full form, the default) or, when
            ``compact=True``, ``CompactRankedSearchResponse``. Both have
            ``backend``, ``total``, ``results`` and a ``compact`` flag that
            tells them apart (``if response.compact:`` narrows the type).

        Compact mode keeps each hit's order, total score, content, tags,
        concepts, refs, relations, ``provenance``, version fields
        (``version_status``, ``successors``, ``current_version``,
        ``successor_of``, ``collapsed_duplicates``) and graph provenance. It
        leaves out the score components, ``matched_terms``, ``match_sources``
        and, on the thought, ``hash``, ``prev_hash``, ``thought_signature``,
        ``signing_key_id``, ``schema_version``, ``agent_name``,
        ``agent_owner`` and empty fields. Those are not attributes of the
        compact types. Use compact for routine recall; use the default form
        when you need a hash or signature to verify a thought, to audit
        provenance, or to debug score components.

        Example:
            >>> client = MentisDbClient()
            >>> response = client.ranked_search(text="job queue", limit=5, compact=True)
            >>> for hit in response.results:
            ...     print(f"[{hit.total_score:.2f}] {hit.thought.content}")
        """
        request_data: dict = {"chain_key": chain_key}
        request_data.update(
            self._filter_payload(
                text=text, limit=limit, offset=offset, thought_types=thought_types,
                roles=roles, tags_any=tags_any, concepts_any=concepts_any,
                agent_ids=agent_ids, agent_names=agent_names, agent_owners=agent_owners,
                min_importance=min_importance, min_confidence=min_confidence,
                since=since, until=until, scope=scope, enable_reranking=enable_reranking,
                rerank_k=rerank_k, entity_type=entity_type,
            )
        )
        request_data.update(
            self._extended_payload(
                graph=graph, as_of=as_of, include_invalidated=include_invalidated,
                df_scope=df_scope, df_cutoffs=df_cutoffs, vector_fusion=vector_fusion,
                resolve_successors=resolve_successors,
                collapse_duplicates=collapse_duplicates, synonym_weight=synonym_weight,
            )
        )
        if compact:
            request_data["compact"] = True

        result = self._post("/v1/ranked-search", request_data)
        if compact:
            return CompactRankedSearchResponse.from_dict(result)
        return RankedSearchResponse.from_dict(result)

    def federated_search(
        self,
        chain_keys: list[str],
        text: Optional[str] = None,
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
        scope: Optional[MemoryScope | str] = None,
        enable_reranking: Optional[bool] = None,
        rerank_k: Optional[int] = None,
        entity_type: Optional[str] = None,
        compact: bool = False,
        *,
        graph: Optional[GraphOptions] = None,
        as_of: Optional[datetime] = None,
        include_invalidated: Optional[bool] = None,
        df_scope: Optional[DfScope | str] = None,
        df_cutoffs: Optional[DfCutoffs] = None,
        vector_fusion: Optional[VectorFusion | str] = None,
        resolve_successors: Optional[bool] = None,
        collapse_duplicates: Optional[float] = None,
        synonym_weight: Optional[float] = None,
    ) -> SearchResponse:
        """
        Run one ranked search across several chains and merge the results.

        The server fuses the per-chain rankings and deduplicates before paging;
        each hit carries the ``chain_key`` it came from.

        Args:
            chain_keys: Chains to search (``POST /v1/federated-search``).
            text: Query text to search for.
            compact: Ask for compact hits; see :meth:`ranked_search` for what
                compact keeps and leaves out. Defaults to ``False``.

        The remaining arguments, including the keyword-only graph, ``as_of``,
        ``include_invalidated`` and ranking knobs, are those of
        :meth:`ranked_search`.

        Returns:
            ``RankedSearchResponse``, or ``CompactRankedSearchResponse`` when
            ``compact=True``.

        Example:
            >>> client = MentisDbClient()
            >>> response = client.federated_search(
            ...     ["backend", "frontend"], text="auth bug", compact=True
            ... )
            >>> for hit in response.results:
            ...     print(hit.chain_key, hit.thought.content)
        """
        request_data: dict = {"chain_keys": list(chain_keys)}
        request_data.update(
            self._filter_payload(
                text=text, limit=limit, offset=offset, thought_types=thought_types,
                roles=roles, tags_any=tags_any, concepts_any=concepts_any,
                agent_ids=agent_ids, agent_names=agent_names, agent_owners=agent_owners,
                min_importance=min_importance, min_confidence=min_confidence,
                since=since, until=until, scope=scope, enable_reranking=enable_reranking,
                rerank_k=rerank_k, entity_type=entity_type,
            )
        )
        request_data.update(
            self._extended_payload(
                graph=graph, as_of=as_of, include_invalidated=include_invalidated,
                df_scope=df_scope, df_cutoffs=df_cutoffs, vector_fusion=vector_fusion,
                resolve_successors=resolve_successors,
                collapse_duplicates=collapse_duplicates, synonym_weight=synonym_weight,
            )
        )
        if compact:
            request_data["compact"] = True

        result = self._post("/v1/federated-search", request_data)
        if compact:
            return CompactRankedSearchResponse.from_dict(result)
        return RankedSearchResponse.from_dict(result)

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
        scope: Optional[MemoryScope | str] = None,
        enable_reranking: Optional[bool] = None,
        rerank_k: Optional[int] = None,
        entity_type: Optional[str] = None,
        *,
        graph: Optional[GraphOptions] = None,
        as_of: Optional[datetime] = None,
        include_invalidated: Optional[bool] = None,
        df_scope: Optional[DfScope | str] = None,
        df_cutoffs: Optional[DfCutoffs] = None,
        vector_fusion: Optional[VectorFusion | str] = None,
        resolve_successors: Optional[bool] = None,
        collapse_duplicates: Optional[float] = None,
        synonym_weight: Optional[float] = None,
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
            graph: ``GraphOptions`` for the supporting-context expansion.
            as_of: Search the chain as it was at this time.
            include_invalidated: Also return superseded, corrected and
                invalidated thoughts (history); the default hides them.
            df_scope: ``DfScope`` of the lexical stop-word gate.
            df_cutoffs: ``DfCutoffs`` overrides of the gate cutoff ratios.
            vector_fusion: ``VectorFusion`` blend of vector and lexical scores.
            resolve_successors: Replace a superseded hit by its live successor
                and promote the thought contesting a contested hit (server
                default ``True``).
            collapse_duplicates: Near-duplicate collapse threshold in (0, 1];
                ``0`` disables (server default 0.9).
            synonym_weight: Weight of automatic thesaurus expansion; ``0``
                disables it (server default 0.3).

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
        serialized_scope = self._serialize_scope(scope)
        if serialized_scope is not None:
            request_data["scope"] = serialized_scope
        if enable_reranking is not None:
            request_data["enable_reranking"] = enable_reranking
        if rerank_k is not None:
            request_data["rerank_k"] = rerank_k
        if entity_type:
            request_data["entity_type"] = entity_type

        request_data.update(
            self._extended_payload(
                graph=graph, as_of=as_of, include_invalidated=include_invalidated,
                df_scope=df_scope, df_cutoffs=df_cutoffs, vector_fusion=vector_fusion,
                resolve_successors=resolve_successors,
                collapse_duplicates=collapse_duplicates, synonym_weight=synonym_weight,
            )
        )

        result = self._post("/v1/context-bundles", request_data)
        bundles = [
            ContextBundle(
                seed=ContextBundleSeed.from_dict(bundle_data["seed"]),
                support=[
                    ContextBundleHit.from_dict(hit_data)
                    for hit_data in bundle_data.get("support", [])
                ],
            )
            for bundle_data in result.get("bundles", [])
        ]
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
