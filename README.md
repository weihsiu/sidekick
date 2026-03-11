# Sidekick

An agentic AI assistant built in Rust with [Synaptic](https://github.com/dnw3/synaptic) and [LanceDB](https://github.com/lancedb/lancedb).

All conversations are persisted in a local LanceDB vector database. Past entries are retrieved via hybrid search (dense vector + full-text keyword matching with Reciprocal Rank Fusion) and injected as context on every turn, giving the agent long-term memory scoped per user.

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

Edit `config.toml` to choose your providers, models, and memory settings. The agent reads this file at startup.

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

### `[memory]` — LanceDB store

| Field | Description |
|-------|-------------|
| `db_path` | Path to the LanceDB database directory on disk |
| `table_name` | Table name within the database |
| `top_k` | Number of relevant entries to retrieve per query |

### `[agent]` — System prompt

| Field | Description |
|-------|-------------|
| `system_prompt` | Base system prompt defining the agent's persona and behavior. RAG context is appended automatically. |

### `[user]` — User identity

| Field | Description |
|-------|-------------|
| `id` | Unique user identifier — partitions memory per user |

### Example configurations

**OpenAI (LLM + embeddings):**

```toml
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
db_path = "data/sidekick.lancedb"
table_name = "conversations"
top_k = 10

[agent]
system_prompt = "You are Sidekick, a helpful AI assistant with long-term memory."

[user]
id = "alice"
```

**Anthropic LLM + OpenAI embeddings:**

```toml
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
db_path = "data/sidekick.lancedb"
table_name = "conversations"
top_k = 10

[agent]
system_prompt = "You are Sidekick, a helpful AI assistant with long-term memory."

[user]
id = "alice"
```

**Fully local with Ollama:**

Ollama runs locally and does not require an API key. Make sure the Ollama server is running before starting sidekick. The default base URL is `http://localhost:11434` — override it if your instance is on a different host/port.

```toml
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
db_path = "data/sidekick.lancedb"
table_name = "conversations"
top_k = 10

[agent]
system_prompt = "You are Sidekick, a helpful AI assistant with long-term memory."

[user]
id = "alice"
```

**Gemini:**

```toml
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
db_path = "data/sidekick.lancedb"
table_name = "conversations"
top_k = 10

[agent]
system_prompt = "You are Sidekick, a helpful AI assistant with long-term memory."

[user]
id = "alice"
```

## Run

Set the appropriate environment variables for your providers, then:

```sh
cargo run --release
```

Type a message at the `>` prompt. Type `quit` or `exit` to stop.

Conversation history is stored in the `db_path` directory and persists across restarts. Each user (identified by `[user] id`) has their own isolated memory.

## Batch import

You can bulk-load entries from a JSONL file:

```sh
# Uses user_id from config.toml
cargo run -- --import data/knowledge.jsonl

# Override user_id on the command line
cargo run -- --import data/knowledge.jsonl --user alice
```

Each line in the JSONL file is a JSON object with the following fields:

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `category` | yes | | Entry category (e.g. `"knowledge"`, `"note"`, `"faq"`) |
| `content` | yes | | The text content |
| `role` | no | `"system"` | Role label (`"system"`, `"human"`, `"ai"`) |

The `user_id` comes from the `--user` flag (or `config.toml`), and the `timestamp` is set to the time of import for all entries in the batch.

**Example `knowledge.jsonl`:**

```jsonl
{"category": "knowledge", "content": "Rust's ownership system prevents data races at compile time."}
{"category": "knowledge", "content": "LanceDB stores data in Lance columnar format on the local filesystem."}
{"category": "faq", "content": "Q: How do I reset my password? A: Go to Settings > Account > Reset Password."}
```

## How it works

1. User sends a message
2. The message is embedded and used to search LanceDB via hybrid search (dense vector cosine similarity + full-text keyword matching, fused with Reciprocal Rank Fusion)
3. Results are filtered by user ID; all categories are searched
4. The configurable system prompt + retrieved context are injected as a system message
5. The LLM generates a response
6. Both the user message and the assistant response are stored in LanceDB (category: `conversation`) with their embeddings

This gives the agent persistent, semantic long-term memory that grows over time and can be seeded with domain knowledge via batch import.

## LanceDB schema

Each entry in the database has the following columns:

| Column | Type | Description |
|--------|------|-------------|
| `id` | string | UUID |
| `user_id` | string | User identifier |
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
  main.rs            # Agentic loop + CLI (--import)
  config.rs          # Config file parsing
  provider.rs        # Builds the ChatModel from config
  embeddings.rs      # Builds the Embeddings model from config
  memory.rs          # LanceDB-backed memory: store, retrieve, batch import
```
