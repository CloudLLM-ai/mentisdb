#!/usr/bin/env python3
"""
LangChain Memory Example for MentisDB.

This example demonstrates how to use MentisDbMemory with LangChain
to store and retrieve conversation memories.

Prerequisites:
    1. Start MentisDB server: mentisdbd
       Or set base_url to your running instance.
    2. Install dependencies: pip install pymentisdb[langchain]

Usage:
    python langchain_memory_example.py
"""

import sys
from typing import Any

# Add parent directory to path for local development
sys.path.insert(0, "..")

from pymentisdb import MentisDbClient, MentisDbMemory, ThoughtType, ThoughtRole
from langchain_core.messages import HumanMessage, AIMessage
from langchain_core.memory import BaseMemory


def basic_client_example():
    """Basic usage of MentisDbClient without LangChain."""
    print("=" * 60)
    print("Basic MentisDbClient Example")
    print("=" * 60)

    # Create client (defaults to http://127.0.0.1:9472)
    client = MentisDbClient()

    # List available chains
    print("\n1. Listing chains...")
    try:
        chains = client.list_chains()
        print(f"   Default chain: {chains.default_chain_key}")
        print(f"   Available chains: {chains.chain_keys}")
    except Exception as e:
        print(f"   (Error listing chains - server may not be running): {e}")
        return

    # Append a thought
    print("\n2. Appending thoughts...")
    thought1 = client.append_thought(
        thought_type=ThoughtType.INSIGHT,
        content="Rate limiting is the real bottleneck for our API.",
        agent_name="assistant",
        importance=0.8,
        tags=["performance", "api"],
    )
    print(f"   Appended: {thought1.id[:8]}... - {thought1.content[:50]}...")

    thought2 = client.append_thought(
        thought_type=ThoughtType.DECISION,
        content="We will implement a sliding window rate limiter.",
        agent_name="assistant",
        importance=0.9,
        tags=["architecture", "api"],
    )
    print(f"   Appended: {thought2.id[:8]}... - {thought2.content[:50]}...")

    thought3 = client.append_thought(
        thought_type=ThoughtType.FINDING,
        content="The cache hit rate dropped to 82% after the last deployment.",
        agent_name="assistant",
        importance=0.7,
        tags=["performance", "cache"],
    )
    print(f"   Appended: {thought3.id[:8]}... - {thought3.content[:50]}...")

    # Search for thoughts
    print("\n3. Searching for 'performance'...")
    results = client.ranked_search(
        text="performance",
        limit=5,
    )
    print(f"   Found {results.total} results:")
    for hit in results.results:
        print(f"   - [{hit.score.total:.3f}] {hit.thought.thought_type.value}: {hit.thought.content[:60]}...")

    # Search with filters
    print("\n4. Searching for 'api' thoughts with high importance...")
    results = client.ranked_search(
        text="api",
        min_importance=0.7,
        tags_any=["architecture"],
    )
    print(f"   Found {results.total} results:")
    for hit in results.results:
        print(f"   - [{hit.score.total:.3f}] {hit.thought.content[:60]}...")

    print("\n" + "=" * 60)
    print("Basic example completed successfully!")
    print("=" * 60)


def langchain_memory_example():
    """LangChain memory integration example."""
    print("\n" + "=" * 60)
    print("LangChain Memory Integration Example")
    print("=" * 60)

    # Create memory instance
    memory = MentisDbMemory(
        base_url="http://127.0.0.1:9472",
        chain_key="langchain-demo",
        agent_name="assistant",
        thought_type=ThoughtType.SUMMARY,
        role=ThoughtRole.MEMORY,
    )

    # Simulate conversation
    conversation = [
        HumanMessage(content="Hi! I'm working on a new project."),
        AIMessage(content="That's exciting! What kind of project is it?"),
        HumanMessage(content="It's a web application for task management."),
        AIMessage(content="Great! What technologies are you using?"),
        HumanMessage(content="Python, FastAPI, and PostgreSQL."),
        AIMessage(content="Solid choices! FastAPI is great for REST APIs."),
        HumanMessage(content="Yes, and I'm using MentisDB for memory."),
        AIMessage(content="Interesting! Are you using the LangChain integration?"),
    ]

    # Add messages to memory
    print("\n1. Adding conversation to memory...")
    memory.add_messages(conversation)
    print(f"   Added {len(conversation)} messages")

    # Load memory variables
    print("\n2. Loading memory variables...")
    memory_vars = memory.load_memory_variables()
    print(f"   Memory key: {list(memory_vars.keys())}")
    print(f"   Memory content preview:")
    for line in memory_vars["chat_history"].split("\n")[:6]:
        print(f"   {line}")

    # Get messages directly
    print("\n3. Getting messages from memory...")
    messages = memory.get_messages()
    print(f"   Retrieved {len(messages)} messages")
    for i, msg in enumerate(messages[:4]):
        msg_type = "Human" if isinstance(msg, HumanMessage) else "Assistant"
        content = msg.content[:50] + "..." if len(msg.content) > 50 else msg.content
        print(f"   {i+1}. {msg_type}: {content}")

    print("\n" + "=" * 60)
    print("LangChain memory example completed!")
    print("=" * 60)


def main():
    """Run all examples."""
    print("\n" + "#" * 60)
    print("# PyMentisDB - LangChain Integration Demo")
    print("#" * 60)

    # Try basic client example
    try:
        basic_client_example()
    except Exception as e:
        print(f"\nBasic example error: {e}")
        print("Make sure MentisDB server is running on http://127.0.0.1:9472")

    # Try LangChain example
    try:
        langchain_memory_example()
    except ImportError as e:
        print(f"\nLangChain example skipped: {e}")
        print("Install with: pip install pymentisdb[langchain]")
    except Exception as e:
        print(f"\nLangChain example error: {e}")


if __name__ == "__main__":
    main()
