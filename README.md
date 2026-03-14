# Sidekick

An agentic AI assistant built in Rust with [Synaptic](https://github.com/dnw3/synaptic) and [LanceDB](https://github.com/lancedb/lancedb).

Sidekick is a Cargo workspace with a Rust backend and a React frontend:

- **`server/`** — Axum HTTP server with OAuth2 authentication, per-user long-term memory (LanceDB), and short-term chat window
- **`client/`** — React + Vite frontend with OAuth login and chat UI

Each user gets their own LanceDB database for complete memory isolation. Past entries are retrieved via hybrid search (dense vector + full-text keyword matching with Reciprocal Rank Fusion) and injected as context on every turn, giving the agent long-term memory per user.

Entries are categorized (e.g. `conversation`, `knowledge`) so you can batch-import structured knowledge alongside organic chat history.

## Prerequisites

- Rust 1.88+
- Node.js 18+
- An API key for your chosen LLM provider (or a running Ollama instance for local use)
- An API key for your chosen embeddings provider
- OAuth2 credentials for at least one provider (Google and/or Facebook)

## Build

**Server:**

```sh
cargo build --release -p sidekick-server
```

All LLM providers and embeddings providers are compiled in. No recompilation is needed to switch between them — just edit `config.toml`.

**Client:**

```sh
cd client
npm install
npm run build
```

## Configuration

Edit `server/config.toml` to choose your providers, models, and memory settings. The server reads this file at startup.

The config file is resolved in this order:

1. `SIDEKICK_CONFIG` environment variable (if set)
2. `config.toml` next to the executable binary
3. `config.toml` in the current working directory

### `[server]` — HTTP server

| Field | Description |
|-------|-------------|
| `host` | Bind address (e.g. `"0.0.0.0"`) |
| `port` | Listen port (e.g. `3000`) |
| `base_url` | Public URL the browser sees, used for OAuth redirect URIs (e.g. `"http://localhost:3000"`) |

### `[auth]` — Authentication

| Field | Description |
|-------|-------------|
| `db_path` | Path to the SQLite database for user identity storage |

### `[auth.providers.<name>]` — OAuth providers

Add a section per provider. Currently supported: `google`, `facebook`. Adding more is just another config section + registering the provider's URLs.

| Field | Description |
|-------|-------------|
| `client_id` | OAuth client ID from the provider |
| `client_secret_env` | Environment variable holding the client secret |
| `auth_url` | Provider's authorization endpoint |
| `token_url` | Provider's token exchange endpoint |
| `userinfo_url` | Provider's userinfo endpoint |
| `scopes` | OAuth scopes to request |

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
base_url = "http://localhost:3000"

[auth]
db_path = "data/sidekick.db"

[auth.providers.google]
client_id = "YOUR_GOOGLE_CLIENT_ID"
client_secret_env = "GOOGLE_CLIENT_SECRET"
auth_url = "https://accounts.google.com/o/oauth2/v2/auth"
token_url = "https://oauth2.googleapis.com/token"
userinfo_url = "https://www.googleapis.com/oauth2/v3/userinfo"
scopes = ["openid", "email", "profile"]

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

**Fully local with Ollama:**

Ollama runs locally and does not require an API key. Make sure the Ollama server is running before starting sidekick. The default base URL is `http://localhost:11434` — override it if your instance is on a different host/port.

```toml
[server]
host = "0.0.0.0"
port = 3000
base_url = "http://localhost:3000"

[auth]
db_path = "data/sidekick.db"

[auth.providers.google]
client_id = "YOUR_GOOGLE_CLIENT_ID"
client_secret_env = "GOOGLE_CLIENT_SECRET"
auth_url = "https://accounts.google.com/o/oauth2/v2/auth"
token_url = "https://oauth2.googleapis.com/token"
userinfo_url = "https://www.googleapis.com/oauth2/v3/userinfo"
scopes = ["openid", "email", "profile"]

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

## OAuth setup

Before running, you need OAuth credentials from at least one provider.

**Google:**

1. Go to [Google Cloud Console > Credentials](https://console.cloud.google.com/apis/credentials)
2. Create an OAuth 2.0 Client ID (type: Web application)
3. Add authorized redirect URI: `http://localhost:3000/auth/google/callback`
4. Put the client ID in `config.toml`, set the secret: `export GOOGLE_CLIENT_SECRET="..."`

**Facebook:**

1. Go to [Facebook Developers](https://developers.facebook.com/apps/)
2. Create an app (type: Consumer), add Facebook Login product
3. Add valid OAuth redirect URI: `http://localhost:3000/auth/facebook/callback`
4. Put the app ID in `config.toml`, set the secret: `export FACEBOOK_CLIENT_SECRET="..."`

## Run

**Development (two terminals):**

```sh
# Terminal 1: start the server
export OPENAI_API_KEY="sk-..."
export GOOGLE_CLIENT_SECRET="..."
cd server
cargo run

# Terminal 2: start the client dev server (hot reload)
cd client
npm run dev
```

Open `http://localhost:5173`. The Vite dev server proxies API requests to the Rust server at `localhost:3000`.

**Production:**

```sh
# Build both
cargo build --release -p sidekick-server
cd client && npm run build

# Run the server
export OPENAI_API_KEY="sk-..."
export GOOGLE_CLIENT_SECRET="..."
SIDEKICK_CONFIG=server/config.toml ./target/release/sidekick-server
```

The client builds to static files in `client/dist/` — serve them with nginx, a CDN, or add static file serving to the Rust server.

Set `RUST_LOG` to control log verbosity:

```sh
RUST_LOG=sidekick_server=debug cargo run -p sidekick-server
```

## API

All API endpoints require authentication (session cookie from OAuth login). Unauthenticated requests return 401. The user is identified from the session — no `user_id` in request bodies.

### Auth routes (public)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/auth/:provider` | Redirect to OAuth provider login (e.g. `/auth/google`) |
| `GET` | `/auth/:provider/callback` | OAuth callback (handles code exchange + session creation) |
| `POST` | `/auth/logout` | Log out (destroy session) |
| `GET` | `/auth/me` | Return current user info, or 401 if not logged in |

### `POST /v1/chat`

Send a message and get a response. The user's message and the assistant's response are both stored in memory automatically.

```json
{
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

Store an entry directly into the current user's memory.

```json
{
  "category": "knowledge",
  "role": "system",
  "content": "Rust's ownership system prevents data races at compile time."
}
```

### `POST /v1/memory/search`

Search the current user's memory without triggering a chat.

```json
{
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
cargo run -p sidekick-server -- --import alice data/knowledge.jsonl
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

- **OAuth2 authentication**: Login via Google or Facebook using PKCE flow. User identity stored in SQLite. Sessions managed by tower-sessions (in-memory). Modular provider config — add new providers by adding a `[auth.providers.<name>]` section.
- **Multi-user**: Each user gets an isolated LanceDB database at `data/users/{user_id}.lancedb`
- **Memory pool**: An LRU cache keeps the most recently active user databases open, evicting idle ones to conserve file descriptors
- **Thread-safe writes**: A per-database mutex serialises store operations and FTS index rebuilds while reads proceed concurrently
- **Dual memory**: Long-term memory via LanceDB hybrid search (RAG) + short-term chat window via Synaptic's `ConversationWindowMemory` for recent conversation continuity
- **Hybrid retrieval**: Dense vector cosine similarity + full-text keyword matching, fused with Reciprocal Rank Fusion (RRF)
- **Reranking**: Retrieved results are reranked with configurable category weight boosts

## How it works

1. User logs in via OAuth (Google/Facebook) — session cookie is set
2. Client sends a `POST /v1/chat` with a `message`
3. The user's database is opened from the pool (or created on first use)
4. The message is embedded and used to search the user's LanceDB via hybrid search (long-term memory)
5. The configurable system prompt + retrieved RAG context are injected as a system message
6. The recent chat window (last N messages) is included as message history for conversational continuity
7. The LLM generates a response with both long-term and short-term context
8. Both the user message and the assistant response are stored in LanceDB (long-term) and the chat window (short-term)
9. The response is returned to the client

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
Cargo.toml                # Workspace root
server/
  Cargo.toml              # sidekick-server package
  config.toml             # Runtime configuration
  src/
    main.rs               # HTTP server, API handlers, --import CLI
    config.rs             # Config file parsing
    error.rs              # API error types (thiserror + anyhow)
    provider.rs           # Builds the ChatModel from config
    embeddings.rs         # Builds the Embeddings model from config
    rerank.rs             # Reranker trait and mock implementation
    memory.rs             # Per-user LanceDB memory: store, retrieve, batch import, pool
    user.rs               # User model (SQLite), AuthUser impl
    auth/
      mod.rs              # AuthBackend for axum-login, AuthnBackend impl
      oauth.rs            # OAuth2 provider (PKCE flow, token exchange, userinfo)
      routes.rs           # Login, callback, logout, me handlers
client/
  package.json            # React + Vite frontend
  vite.config.ts          # Dev server with API proxy to backend
  src/
    main.tsx              # App entry, routing (login vs chat)
    auth.tsx              # AuthProvider context (session check via /auth/me)
    pages/
      Login.tsx           # OAuth login buttons (Google, Facebook)
      Chat.tsx            # Chat interface
    styles.css            # Styling
```
