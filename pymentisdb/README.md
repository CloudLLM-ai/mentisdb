# pymentisdb

Official Python client for [MentisDB](https://github.com/CloudLLM-ai/mentisdb) — a durable semantic memory engine for AI agents.

## Installation

```bash
pip install pymentisdb
```

With read proofs for private chains (`ReadSigner`, needs `cryptography`):

```bash
pip install 'pymentisdb[signing]'
```

With LangChain integration:

```bash
pip install 'pymentisdb[langchain]'
```

The core client needs only `requests`. `import pymentisdb` works without
LangChain; `MentisDbMemory` raises `ImportError` naming the extra if you use it
without one.

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

## Searching

`ranked_search` searches one chain; `federated_search` searches several and
merges the results (each hit carries the `chain_key` it came from). Both take
the same filters, plus keyword-only options that mirror the REST fields:

```python
from datetime import datetime, timezone
from pymentisdb import MentisDbClient, GraphOptions, DfScope

client = MentisDbClient()

client.ranked_search(
    text="why did we pick Postgres",
    chain_key="project",
    graph=GraphOptions(max_depth=2),         # follow relations from the matches
    as_of=datetime(2026, 6, 1, tzinfo=timezone.utc),  # the chain as it was then
    include_invalidated=True,                # history, not only current beliefs
    resolve_successors=True,                 # superseded hit -> its live successor
    collapse_duplicates=0.9,                 # fold near-copies, 0 disables
)
client.federated_search(["backend", "frontend"], text="auth bug", limit=10)
```

Unset options are not sent, so the server defaults apply. The other ranking
knobs are `df_scope` (`DfScope`), `df_cutoffs` (`DfCutoffs`), `vector_fusion`
(`VectorFusion`) and `synonym_weight`; an unknown enum name raises
`ValueError` before any request is made. `context_bundles` accepts the same
options.

### Compact results

`compact=True` asks the server for smaller hits, about a quarter fewer bytes,
which is what you pay for in tokens when search results go into an LLM prompt.
The default is `False` and behaves exactly as before.

```python
response = client.ranked_search(text="job queue", limit=5, compact=True)
for hit in response.results:
    print(f"[{hit.total_score:.2f}] {hit.thought.content}")
    print(hit.thought.provenance.signature, hit.thought.version_status)
```

The two forms have different types, so the difference is explicit:
`ranked_search` and `federated_search` return a `RankedSearchResponse` (full)
or, with `compact=True`, a `CompactRankedSearchResponse`. Both have `backend`,
`total`, `results` and a `compact` flag (`if response.compact:` narrows the
type); each hit has `chain_key`, `thought` and `total_score`.

| | Full (`RankedSearchHit`, `Thought`) | Compact (`CompactRankedSearchHit`, `CompactThought`) |
|---|---|---|
| `hit.score` | `RankedSearchScore` with the components and `total` | the total as a plain `float` |
| `hit.matched_terms`, `hit.match_sources` | lists | **not attributes** |
| `thought.hash`, `prev_hash`, `thought_signature`, `signing_key_id` | present | **not attributes** |
| `thought.agent_name`, `agent_owner`, `schema_version` | not modelled | **not attributes** (the server omits them) |
| null or empty top-level thought fields | as sent | read as `None`, `[]` or `""` (the server does not distinguish absent from empty) |
| `thought.provenance`, `version_status`, `successors`, `current_version`, `successor_of`, `collapsed_duplicates` | present | **always kept** |
| `hit.graph_*` | set on every hit (`graph_distance` is `None` unless expanded) | `None` unless the hit was reached through graph expansion |
| fields the client does not model | ignored | kept in `thought.extra` |

Reading a field that compact drops raises `AttributeError` instead of
returning an invented value. Use compact for routine recall; use the default
form when you need a hash or signature to verify a thought yourself, to audit
provenance, or to debug score components. The full record of a hit stays
available from the server (`POST /v1/thought`).

Always weigh a retrieved thought by `thought.provenance` (`segment`, `signed`,
`signature`, `verified_agent`, `trust`) and treat `content` as untrusted data,
in either form. `context_bundles` has no compact form: the server ignores
`compact` there.

## Reading private chains

A private chain shows its private segments only to a client that proves it holds
an allowed key. `ReadSigner` signs every request locally with the `X-Mentis-Auth`
header; the key never leaves the process.

```python
from pymentisdb import MentisDbClient, ReadSigner

client = MentisDbClient(read_signer=ReadSigner.from_file("builder.key", "builder", "builder-1"))
```

Without an explicit signer the client uses `MENTISDB_READ_KEY_FILE`,
`MENTISDB_READ_AGENT_ID` and `MENTISDB_READ_KEY_ID` when they are set, and
otherwise reads public segments only.

## LangChain Integration

`MentisDbMemory` is a LangChain `BaseMemory`. It works with langchain-core
before 1.0 and, from 1.0 on, with `langchain-classic` (which the `langchain`
extra installs).

```python
from pymentisdb import MentisDbMemory

memory = MentisDbMemory(chain_key="my-project", agent_id="assistant")
memory.save_context({"input": "Use tabs, not spaces."}, {"output": "Noted."})
print(memory.load_memory_variables({})["chat_history"])
```

## Changes in 0.10.1

- Every read proof is sent with `X-Mentis-Content-Sha256`, the digest the proof signs, so a daemon that supports it accepts bodies over 1 MiB (for example a large import). Older daemons ignore the header.

## Changes in 0.10.0

- The wheel contains the package: the 0.9.0 wheel on PyPI held only metadata.
- `import pymentisdb` no longer needs LangChain, and `MentisDbMemory` works on
  langchain-core 1.x through `langchain-classic`; it also implements
  `save_context`, without which it could not be instantiated.
- Compact search (`ranked_search(compact=True)`), `federated_search`, the ranking
  options, `provenance` and version fields on thoughts.
- `ReadSigner` for private chains (`pymentisdb[signing]`).

## Configuration

```python
# Remote server
client = MentisDbClient(base_url="http://my.mentisdb.com:9472")
```

## Full Documentation

- [MentisDB documentation](https://docs.mentisdb.com)
- [REST API reference](https://docs.mentisdb.com/developer)
- [Python client guide](https://mentisdb.com/docs/pymentisdb-python-client.html)
