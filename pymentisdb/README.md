# pymentisdb

Official Python client for [MentisDB](https://github.com/CloudLLM-ai/mentisdb) — a durable semantic memory engine for AI agents.

## Installation

```bash
pip install pymentisdb
```

With LangChain integration:

```bash
pip install pymentisdb[langchain]
```

## Quick Start

```python
from pymentisdb import MentisDbClient, ThoughtType

client = MentisDbClient()

# Append a thought
thought = client.append_thought(
    thought_type=ThoughtType.INSIGHT,
    content="Rate limiting is the real bottleneck."
)
print(f"Appended: {thought.id}")

# Semantic search
results = client.ranked_search(text="performance optimization")
for hit in results.results:
    print(f"[{hit.score.total:.3f}] {hit.thought.content}")

# Context bundles
bundles = client.context_bundles(text="cache invalidation", limit=5)
```

## LangChain Integration

```python
from pymentisdb import MentisDbMemory
from langchain_openai import ChatOpenAI

memory = MentisDbMemory(
    chain_key="my-project",
    agent_id="assistant"
)

llm = ChatOpenAI(model="gpt-4")
chain = llm.with_memory(memory)
```

## Configuration

```python
# Remote server
client = MentisDbClient(base_url="http://my.mentisdb.com:9472")
```

## Full Documentation

- [MentisDB documentation](https://docs.mentisdb.com)
- [REST API reference](https://docs.mentisdb.com/developer)
- [Python client guide](https://mentisdb.com/docs/pymentisdb-python-client.html)
