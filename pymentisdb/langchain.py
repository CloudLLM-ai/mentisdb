"""
MentisDbMemory - LangChain BaseMemory interface for MentisDB.

This module provides a LangChain-compatible memory class that stores conversation
memories in MentisDB using its REST API.
"""

from typing import Any, Optional
from langchain_core.memory import BaseMemory
from langchain_core.messages import HumanMessage, AIMessage, BaseMessage
from pydantic import Field

from .client import MentisDbClient
from .types import ThoughtType, ThoughtRole, MemoryScope


class MentisDbMemory(BaseMemory):
    """
    LangChain BaseMemory implementation backed by MentisDB.

    This memory class allows LangChain agents to store and retrieve conversation
    memories using MentisDB's append-only semantic memory system.

    Args:
        base_url: Base URL of the MentisDB REST server.
                 Defaults to "http://127.0.0.1:9472".
        chain_key: Optional chain identifier. Uses default chain if not provided.
        agent_id: Optional agent identifier for attribution.
        agent_name: Optional human-readable agent name.
        session_id: Optional session identifier to group related memories.
        thought_type: ThoughtType for appended memories.
                     Defaults to ThoughtType.SUMMARY.
        role: ThoughtRole for appended memories.
              Defaults to ThoughtRole.MEMORY.
        scope: MemoryScope visibility for appended memories.
               Defaults to MemoryScope.USER.
        memory_key: Key in LangChain state dict where memory is stored.
                    Defaults to "chat_history".

    Example:
        >>> from langchain_openai import ChatOpenAI
        >>> from langchain_core.prompts import ChatPromptTemplate
        >>> from langchain_core.runnables import RunnableWithMessageHistory
        >>> from langchain_community.chat_message_histories import ChatMessageHistory
        >>>
        >>> memory = MentisDbMemory(chain_key="my-agent")
        >>> llm = ChatOpenAI(model="gpt-4")
        >>> prompt = ChatPromptTemplate.from_messages([
        ...     ("system", "You are a helpful assistant."),
        ...     ("placeholder", "{chat_history}"),
        ...     ("human", "{question}"),
        ... ])
        >>>
        >>> chain = prompt | llm
        >>> chain_with_memory = chain.with_message_history(
        ...     ChatMessageHistory(session_id="user-123"),
        ... )
    """

    base_url: str = Field(default="http://127.0.0.1:9472")
    chain_key: Optional[str] = Field(default=None)
    agent_id: Optional[str] = Field(default=None)
    agent_name: Optional[str] = Field(default=None)
    session_id: Optional[str] = Field(default=None)
    thought_type: ThoughtType = Field(default=ThoughtType.SUMMARY)
    role: ThoughtRole = Field(default=ThoughtRole.MEMORY)
    scope: MemoryScope = Field(default=MemoryScope.USER)
    memory_key: str = Field(default="chat_history")

    _client: Optional[MentisDbClient] = None

    def __init__(
        self,
        base_url: str = "http://127.0.0.1:9472",
        chain_key: Optional[str] = None,
        agent_id: Optional[str] = None,
        agent_name: Optional[str] = None,
        session_id: Optional[str] = None,
        thought_type: ThoughtType = ThoughtType.SUMMARY,
        role: ThoughtRole = ThoughtRole.MEMORY,
        scope: MemoryScope = MemoryScope.USER,
        memory_key: str = "chat_history",
        **kwargs: Any,
    ):
        super().__init__(
            base_url=base_url,
            chain_key=chain_key,
            agent_id=agent_id,
            agent_name=agent_name,
            session_id=session_id,
            thought_type=thought_type,
            role=role,
            scope=scope,
            memory_key=memory_key,
            **kwargs,
        )
        self._client = MentisDbClient(base_url=base_url)

    @property
    def client(self) -> MentisDbClient:
        """Get or create the MentisDB client."""
        if self._client is None:
            self._client = MentisDbClient(base_url=self.base_url)
        return self._client

    @property
    def memory_variables(self) -> list[str]:
        """
        Return list of memory variables this memory class provides.

        Returns:
            List containing the memory key.
        """
        return [self.memory_key]

    def load_memory_variables(self, inputs: Optional[dict[str, Any]] = None) -> dict[str, Any]:
        """
        Load memory variables from the chat history.

        Retrieves recent thoughts from MentisDB and formats them as a
        string suitable for inclusion in a prompt.

        Args:
            inputs: Optional input dict (unused, for compatibility).

        Returns:
            Dict with memory key mapping to formatted chat history.
        """
        messages = self.get_messages()
        if not messages:
            return {self.memory_key: ""}

        formatted = self._format_messages(messages)
        return {self.memory_key: formatted}

    def _format_messages(self, messages: list[BaseMessage]) -> str:
        """
        Format messages as a string for prompts.

        Args:
            messages: List of BaseMessage objects.

        Returns:
            Formatted string representation of messages.
        """
        lines = []
        for msg in messages:
            if isinstance(msg, HumanMessage):
                role = "Human"
            elif isinstance(msg, AIMessage):
                role = "Assistant"
            else:
                role = "Message"
            lines.append(f"{role}: {msg.content}")
        return "\n".join(lines)

    def add_messages(self, messages: list[BaseMessage]) -> None:
        """
        Add messages to memory by appending thoughts to MentisDB.

        Each message is converted to a Thought with:
        - HumanMessage -> ThoughtType.SUMMARY with role Memory
        - AIMessage -> ThoughtType.SUMMARY with role Memory

        Args:
            messages: List of messages to add to memory.

        Raises:
            requests.HTTPError: If the API request fails.
        """
        for msg in messages:
            content = msg.content
            if not content or not isinstance(content, str):
                continue

            self.client.append_thought(
                thought_type=self.thought_type,
                content=content,
                chain_key=self.chain_key,
                agent_id=self.agent_id,
                agent_name=self.agent_name,
                role=self.role,
                scope=self.scope,
            )

    def get_messages(self) -> list[BaseMessage]:
        """
        Retrieve recent thoughts from MentisDB as LangChain messages.

        Performs a ranked search to retrieve recent memories and
        converts them to HumanMessage/AIMessage pairs.

        Returns:
            List of BaseMessage objects representing recent memory.
        """
        try:
            result = self.client.ranked_search(
                chain_key=self.chain_key,
                limit=20,
                thought_types=[self.thought_type],
            )
        except Exception:
            return []

        messages = []
        for hit in result.results:
            content = hit.thought.content
            if not content:
                continue
            if hit.thought.agent_id == self.agent_id:
                messages.append(AIMessage(content=content))
            else:
                messages.append(HumanMessage(content=content))

        return messages

    def clear(self) -> None:
        """
        Clear memory state.

        Note: This implementation does not support true deletion from
        MentisDB since it is append-only. This method is a no-op for
        safety. To "clear" memory, consider using a new session_id or
        chain_key instead.
        """
        pass

    def _get_session_id(self) -> Optional[str]:
        """Get the session ID for grouping memories."""
        return self.session_id
