# Sidekick

An agentic AI assistant built in Rust with [Synaptic](https://github.com/dnw3/synaptic) and [LanceDB](https://github.com/lancedb/lancedb).

Sidekick runs as an HTTP server serving multiple users. Each user gets their own LanceDB database for complete memory isolation. Past entries are retrieved via hybrid search (dense vector + full-text keyword matching with Reciprocal Rank Fusion) and injected as context on every turn, giving the agent long-term memory per user.

Entries are categorized (e.g. `conversation`, `knowledge`) so you can batch-import structured knowledge alongside organic chat history.

## Prerequisites

- Rust 1.88+
- An API key for your chosen LLM provider (or a running Ollama instance for local use)
- An API key for your chosen embeddings provider

## Build

```sh
cargo build --release
```

All LLM providers and embeddings providers are compiled in. No recompilation is needed to switch between them — just edit `config.toml`.

## Configuration

Edit `config.toml` to choose your providers, models, and memory settings. The server reads this file at startup.

### `[server]` — HTTP server

| Field | Description |
|-------|-------------|
| `host` | Bind address (e.g. `"0.0.0.0"`) |
| `port` | Listen port (e.g. `3000`) |

### `[llm]` — Chat model

| Field | Description |
|-------|-------------|
| `provider` | `"openai"`, `"anthropic"`, `"gemini"`, or `"ollama"` |
| `model` | Model name (e.g. `"gpt-4o"`, `"claude-sonnet-4-20250514"`, `"gemini-2.0-flash"`, `"llama3"`) |
| `api_key_env` | Environment variable holding the API key |
| `base_url` | *(optional)* Override the API endpoint |

### `[embeddings]` — Embedding model

Used to generate vector embeddings for memory storage and retrieval.

| Field | Description |
|-------|-------------|
| `provider` | `"openai"` or `"ollama"` |
| `model` | Model name (e.g. `"text-embedding-3-small"`, `"nomic-embed-text"`) |
| `api_key_env` | Environment variable holding the API key |
| `base_url` | *(optional)* Override the API endpoint |
| `dimensions` | Embedding vector size (must match the model — e.g. `1536` for `text-embedding-3-small`) |

### `[memory]` — Per-user LanceDB store

Each user gets their own database at `{base_path}/{user_id}.lancedb`.

| Field | Description |
|-------|-------------|
| `base_path` | Base directory for per-user LanceDB databases |
| `table_name` | Table name within each database |
| `top_k` | Number of relevant entries to retrieve per query |
| `pool_size` | Max number of user DBs to keep open in the LRU pool |
| `chat_window` | Number of recent chat messages to keep as short-term conversation context |

### `[rerank]` — Result reranking

| Field | Description |
|-------|-------------|
| `provider` | `"mock"` for pass-through (future: `"cohere"`, `"jina"`) |
| `top_n` | Number of top results to keep after reranking |

### `[rerank.category_weights]` — Category boost multipliers

Categories not listed default to `1.0`.

### `[agent]` — System prompt

| Field | Description |
|-------|-------------|
| `system_prompt` | Base system prompt defining the agent's persona and behavior. RAG context is appended automatically. |

### Example configurations

**OpenAI (LLM + embeddings):**

```toml
[server]
host = "0.0.0.0"
port = 3000

[llm]
provider = "openai"
model = "gpt-4o"
api_key_env = "OPENAI_API_KEY"

[embeddings]
provider = "openai"
model = "text-embedding-3-small"
api_key_env = "OPENAI_API_KEY"
dimensions = 1536

[memory]
base_path = "data/users"
table_name = "conversations"
top_k = 10
pool_size = 100
chat_window = 20

[rerank]
provider = "mock"
top_n = 5

[agent]
system_prompt = "You are Sidekick, a helpful AI assistant with long-term memory."
```

**Anthropic LLM + OpenAI embeddings:**

```toml
[server]
host = "0.0.0.0"
port = 3000

[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"

[embeddings]
provider = "openai"
model = "text-embedding-3-small"
api_key_env = "OPENAI_API_KEY"
dimensions = 1536

[memory]
base_path = "data/users"
table_name = "conversations"
top_k = 10
pool_size = 100
chat_window = 20

[rerank]
provider = "mock"
top_n = 5

[agent]
system_prompt = "You are Sidekick, a helpful AI assistant with long-term memory."
```

**Fully local with Ollama:**

Ollama runs locally and does not require an API key. Make sure the Ollama server is running before starting sidekick. The default base URL is `http://localhost:11434` — override it if your instance is on a different host/port.

```toml
[server]
host = "0.0.0.0"
port = 3000

[llm]
provider = "ollama"
model = "llama3"
api_key_env = "UNUSED"
# base_url = "http://192.168.1.100:11434"

[embeddings]
provider = "ollama"
model = "nomic-embed-text"
api_key_env = "UNUSED"
# base_url = "http://192.168.1.100:11434"
dimensions = 768

[memory]
base_path = "data/users"
table_name = "conversations"
top_k = 10
pool_size = 100
chat_window = 20

[rerank]
provider = "mock"
top_n = 5

[agent]
system_prompt = "You are Sidekick, a helpful AI assistant with long-term memory."
```

**Gemini:**

```toml
[server]
host = "0.0.0.0"
port = 3000

[llm]
provider = "gemini"
model = "gemini-2.0-flash"
api_key_env = "GEMINI_API_KEY"

[embeddings]
provider = "openai"
model = "text-embedding-3-small"
api_key_env = "OPENAI_API_KEY"
dimensions = 1536

[memory]
base_path = "data/users"
table_name = "conversations"
top_k = 10
pool_size = 100
chat_window = 20

[rerank]
provider = "mock"
top_n = 5

[agent]
system_prompt = "You are Sidekick, a helpful AI assistant with long-term memory."
```

## Run

Set the appropriate environment variables for your providers, then:

```sh
cargo run --release
```

The server starts on the configured host and port. Set `RUST_LOG` to control log verbosity:

```sh
RUST_LOG=sidekick=debug cargo run --release
```

## API

All endpoints accept and return JSON.

### `POST /v1/chat`

Send a message and get a response. The user's message and the assistant's response are both stored in memory automatically.

```json
{
  "user_id": "alice",
  "message": "What did we talk about yesterday?"
}
```

Response:

```json
{
  "response": "Yesterday we discussed..."
}
```

### `POST /v1/memory/store`

Store an entry directly into a user's memory.

```json
{
  "user_id": "alice",
  "category": "knowledge",
  "role": "system",
  "content": "Rust's ownership system prevents data races at compile time."
}
```

### `POST /v1/memory/search`

Search a user's memory without triggering a chat.

```json
{
  "user_id": "alice",
  "query": "Rust ownership",
  "categories": ["knowledge"]
}
```

Response:

```json
{
  "entries": [
    {
      "category": "knowledge",
      "role": "system",
      "content": "Rust's ownership system prevents data races at compile time.",
      "timestamp": "2025-01-15T10:30:00+00:00"
    }
  ]
}
```

## Batch import

You can bulk-load entries from a JSONL file for a specific user:

```sh
cargo run -- --import alice data/knowledge.jsonl
```

Each line in the JSONL file is a JSON object with the following fields:

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `category` | yes | | Entry category (e.g. `"knowledge"`, `"note"`, `"faq"`) |
| `content` | yes | | The text content |
| `role` | no | `"system"` | Role label (`"system"`, `"human"`, `"ai"`) |

The `timestamp` is set to the time of import for all entries in the batch.

**Example `knowledge.jsonl`:**

```jsonl
{"category": "knowledge", "content": "Rust's ownership system prevents data races at compile time."}
{"category": "knowledge", "content": "LanceDB stores data in Lance columnar format on the local filesystem."}
{"category": "faq", "content": "Q: How do I reset my password? A: Go to Settings > Account > Reset Password."}
```

## Architecture

- **Multi-user**: Each user gets an isolated LanceDB database at `data/users/{user_id}.lancedb`
- **Memory pool**: An LRU cache keeps the most recently active user databases open, evicting idle ones to conserve file descriptors
- **Thread-safe writes**: A per-database mutex serialises store operations and FTS index rebuilds while reads proceed concurrently
- **Dual memory**: Long-term memory via LanceDB hybrid search (RAG) + short-term chat window via Synaptic's `ConversationWindowMemory` for recent conversation continuity
- **Hybrid retrieval**: Dense vector cosine similarity + full-text keyword matching, fused with Reciprocal Rank Fusion (RRF)
- **Reranking**: Retrieved results are reranked with configurable category weight boosts

## How it works

1. Client sends a `POST /v1/chat` with a `user_id` and `message`
2. The user's database is opened from the pool (or created on first use)
3. The message is embedded and used to search the user's LanceDB via hybrid search (long-term memory)
4. The configurable system prompt + retrieved RAG context are injected as a system message
5. The recent chat window (last N messages) is included as message history for conversational continuity
6. The LLM generates a response with both long-term and short-term context
7. Both the user message and the assistant response are stored in LanceDB (long-term) and the chat window (short-term)
8. The response is returned to the client

## LanceDB schema

Each entry in a user's database has the following columns:

| Column | Type | Description |
|--------|------|-------------|
| `id` | string | UUID |
| `category` | string | Entry category (`conversation`, `knowledge`, etc.) |
| `role` | string | `human`, `ai`, or `system` |
| `content` | string | Text content |
| `timestamp` | string | ISO 8601 timestamp |
| `vector` | float32[] | Dense embedding vector |

A full-text search index on `content` is maintained automatically for hybrid retrieval.

## Project structure

```
config.toml          # All runtime configuration
src/
  main.rs            # HTTP server, API handlers, --import CLI
  config.rs          # Config file parsing
  provider.rs        # Builds the ChatModel from config
  embeddings.rs      # Builds the Embeddings model from config
  rerank.rs          # Reranker trait and mock implementation
  memory.rs          # Per-user LanceDB memory: store, retrieve, batch import, pool
```
