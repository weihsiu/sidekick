# Sidekick

An agentic AI assistant built in Rust with [Synaptic](https://github.com/dnw3/synaptic) and [LanceDB](https://github.com/lancedb/lancedb).

Sidekick has a Rust backend and a React frontend, packaged as a PWA:

- **`server/`** — Axum HTTP server with OAuth2 authentication, per-user dual-store memory (LanceDB for semantic search + SQLite for browsable history), and short-term chat window. The client is embedded into the server binary via `rust-embed`, producing a single self-contained executable.
- **`client/`** — React + Vite PWA with OAuth login, chat UI, and infinite scroll conversation history

Each user gets their own LanceDB database for semantic search and a SQLite database for ordered history, both managed under a single LRU pool for file descriptor efficiency. Past entries are retrieved via hybrid search (dense vector + full-text keyword matching with Reciprocal Rank Fusion) and injected as context on every turn, giving the agent long-term memory per user.

Entries are categorized (e.g. `conversation`, `knowledge`) so you can batch-import structured knowledge alongside organic chat history. All writes go to both stores — LanceDB for semantic retrieval, SQLite for chronological browsing.

## Prerequisites

- Rust 1.88+
- Node.js 18+
- An API key for your chosen LLM provider (or a running Ollama instance for local use)
- An API key for your chosen embeddings provider
- OAuth2 credentials for at least one provider (Google and/or Facebook)

## Build

Build the client first, then the server. The server embeds `client/dist/` into the binary at compile time.

```sh
# 1. Build the client
cd client
npm install
npm run build

# 2. Build the server (embeds client/dist/ into the binary)
cd ../server
cargo build --release
```

All LLM providers and embeddings providers are compiled in. No recompilation is needed to switch between them — just edit `config.toml`.

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

### `[memory]` — Per-user memory stores

Each user gets two databases under `{base_path}/`:
- `{user_id}.lancedb` — LanceDB for semantic/vector search
- `{user_id}.memory.db` — SQLite for ordered, browsable history

Both are managed under a single LRU pool so they are opened and evicted together.

| Field | Description |
|-------|-------------|
| `base_path` | Base directory for per-user databases |
| `table_name` | Table name within each LanceDB database |
| `top_k` | Number of relevant entries to retrieve per query |
| `pool_size` | Max number of user database pairs to keep open in the LRU pool |
| `chat_window` | Number of recent chat messages to keep as short-term context |

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
table_name = "memory"
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
table_name = "memory"
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

### Environment variables

| Variable | Required | Description |
|----------|----------|-------------|
| `OPENAI_API_KEY` | depends on provider | API key for OpenAI (LLM and/or embeddings) |
| `GOOGLE_CLIENT_SECRET` | if using Google OAuth | OAuth client secret for Google |
| `FACEBOOK_CLIENT_SECRET` | if using Facebook OAuth | OAuth client secret for Facebook |
| `SESSION_SECRET` | no | Encryption key for the "remember me" cookie. If not set, a random key is generated on each startup (cookies won't survive restarts). Use a stable, long random string in production. |
| `SIDEKICK_CONFIG` | no | Path to the config file (overrides default resolution) |
| `RUST_LOG` | no | Log level filter (e.g. `sidekick_server=debug`) |

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

**Production (single binary):**

```sh
# Build client, then server
cd client && npm install && npm run build && cd ..
cd server && cargo build --release

# Run — just the one binary, no separate file serving needed
export OPENAI_API_KEY="sk-..."
export GOOGLE_CLIENT_SECRET="..."
./target/release/sidekick-server
```

Open `http://localhost:3000`. The client is served directly from the embedded assets. The app is a PWA — it can be installed to the home screen on mobile or as a desktop app in supported browsers.

Set `RUST_LOG` to control log verbosity:

```sh
cd server
RUST_LOG=sidekick_server=debug cargo run
```

## API

All API endpoints require authentication (session cookie from OAuth login). Unauthenticated requests return 401. The user is identified from the session — no `user_id` in request bodies.

### Auth routes (public)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/auth/:provider` | Redirect to OAuth provider login (e.g. `/auth/google`) |
| `GET` | `/auth/:provider/callback` | OAuth callback (handles code exchange + session creation) |
| `POST` | `/auth/logout` | Log out (destroy session + clear remember cookie) |
| `GET` | `/auth/me` | Return current user info. Restores session from remember cookie if expired. Returns 401 if not logged in. |

### `POST /v1/chat`

Send a message and get a response. The user's message and the assistant's response are both stored in memory automatically (both LanceDB and SQLite).

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

Store an entry directly into the current user's memory (both LanceDB and SQLite).

```json
{
  "category": "knowledge",
  "role": "system",
  "content": "Rust's ownership system prevents data races at compile time."
}
```

### `POST /v1/memory/search`

Search the current user's semantic memory (LanceDB) without triggering a chat.

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

### `GET /v1/history`

Browse memory entries with cursor-based pagination (infinite scroll). Returns entries in ascending order within each page.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `before` | integer | *(latest)* | Cursor — return entries with ID less than this value |
| `limit` | integer | `20` | Max entries to return (capped at 100) |
| `category` | string | *(all)* | Filter by category (e.g. `conversation`, `knowledge`) |

Response:

```json
[
  {
    "id": 41,
    "category": "conversation",
    "role": "human",
    "content": "What is Rust?",
    "timestamp": "2025-01-15T10:30:00+00:00"
  },
  {
    "id": 42,
    "category": "conversation",
    "role": "ai",
    "content": "Rust is a systems programming language...",
    "timestamp": "2025-01-15T10:30:00+00:00"
  }
]
```

To paginate, pass the smallest `id` from the current page as the `before` parameter in the next request.

## Batch import

You can bulk-load entries from a JSONL file for a specific user. Entries are written to both LanceDB (for semantic search) and SQLite (for browsable history).

```sh
cd server
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

## Deploy to Fly.io

Deployment files are in `server/flyio/`. The deploy script builds a Docker image locally for `linux/amd64` and pushes it to Fly.io's registry.

### First-time setup

1. Install the [Fly CLI](https://fly.io/docs/flyctl/install/) and log in:

   ```sh
   fly auth login
   ```

2. Create the app:

   ```sh
   fly apps create sidekick-server
   ```

3. Create a persistent volume for SQLite and LanceDB data:

   ```sh
   fly volumes create sidekick_data --region sjc --size 1 --app sidekick-server
   ```

4. Set secrets:

   ```sh
   fly secrets set \
     GOOGLE_CLIENT_SECRET="..." \
     FACEBOOK_CLIENT_SECRET="..." \
     OPENAI_API_KEY="sk-..." \
     SESSION_SECRET="your-long-random-secret-string" \
     --app sidekick-server
   ```

5. Update `server/config.toml`:
   - Set `base_url` and `frontend_url` to your Fly.io URL (e.g. `https://sidekick-server.fly.dev`)
   - Update the OAuth redirect URIs in Google Cloud Console and Facebook Developer settings to match (e.g. `https://sidekick-server.fly.dev/auth/google/callback`)

### Deploy

From the project root:

```sh
./server/flyio/deploy.sh
```

This builds the Docker image with `--platform=linux/amd64`, pushes it to Fly.io's registry, and deploys.

### Fly.io configuration

The `server/flyio/fly.toml` configures:
- Region: `sjc` (edit `primary_region` to change)
- Persistent volume mounted at `/app/data` for SQLite + LanceDB
- HTTPS enforced, auto-stop/start machines
- 512MB shared CPU VM

## Architecture

- **OAuth2 authentication**: Login via Google or Facebook using PKCE flow. User identity stored in SQLite. Sessions managed by tower-sessions (in-memory) with an encrypted "remember me" cookie that survives server restarts and browser closes (30-day expiry). Modular provider config — add new providers by adding a `[auth.providers.<name>]` section.
- **Multi-user**: Each user gets isolated databases at `data/users/{user_id}.lancedb` (semantic) and `data/users/{user_id}.memory.db` (history)
- **Dual-store memory**: LanceDB for semantic/vector search (RAG retrieval) + SQLite for ordered, cursor-based browsing (infinite scroll). All writes go to both stores.
- **Memory pool**: A single LRU cache manages both databases per user as one unit, evicting idle users to conserve file descriptors
- **Thread-safe writes**: A per-database mutex serialises store operations and FTS index rebuilds while reads proceed concurrently
- **Chat window**: Short-term context via Synaptic's `ConversationWindowMemory` for recent conversational continuity
- **Hybrid retrieval**: Dense vector cosine similarity + full-text keyword matching, fused with Reciprocal Rank Fusion (RRF)
- **Reranking**: Retrieved results are reranked with configurable category weight boosts
- **PWA**: Installable progressive web app with service worker caching for offline static assets

## How it works

1. User logs in via OAuth (Google/Facebook) — session cookie and encrypted "remember me" cookie are set
2. Client loads recent conversation history from SQLite via `GET /v1/history` (infinite scroll)
3. Client sends a `POST /v1/chat` with a `message`
4. The user's databases are opened from the pool (or created on first use)
5. The message is embedded and used to search the user's LanceDB via hybrid search (long-term semantic memory)
6. The configurable system prompt + retrieved RAG context are injected as a system message
7. The recent chat window (last N messages) is included as message history for conversational continuity
8. The LLM generates a response with both long-term and short-term context
9. Both the user message and the assistant response are stored in LanceDB (semantic) and SQLite (history), plus the chat window (short-term)
10. The response is returned to the client

## Data stores

### LanceDB (semantic memory)

Each entry in a user's LanceDB has the following columns:

| Column | Type | Description |
|--------|------|-------------|
| `id` | string | UUID |
| `category` | string | Entry category (`conversation`, `knowledge`, etc.) |
| `role` | string | `human`, `ai`, or `system` |
| `content` | string | Text content |
| `timestamp` | string | ISO 8601 timestamp |
| `vector` | float32[] | Dense embedding vector |

A full-text search index on `content` is maintained automatically for hybrid retrieval.

### SQLite (memory history)

Each entry in a user's SQLite database:

| Column | Type | Description |
|--------|------|-------------|
| `id` | integer | Auto-increment primary key (used as pagination cursor) |
| `category` | string | Entry category (`conversation`, `knowledge`, etc.) |
| `role` | string | `human`, `ai`, or `system` |
| `content` | string | Text content |
| `timestamp` | string | ISO 8601 timestamp |

## Project structure

```
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
    memory.rs             # Per-user memory: SemanticMemory (LanceDB), UserMemory bundle, pool
    history.rs            # Per-user memory history: MemoryHistory (SQLite), cursor pagination
    user.rs               # User model (SQLite), AuthUser impl
    auth/
      mod.rs              # AuthBackend for axum-login, AuthnBackend impl
      oauth.rs            # OAuth2 provider (PKCE flow, token exchange, userinfo)
      routes.rs           # Login, callback, logout, me handlers
client/
  package.json            # React + Vite PWA frontend
  vite.config.ts          # Dev server with API proxy to backend
  public/
    manifest.json         # PWA manifest
    sw.js                 # Service worker for offline caching
    icons/                # PWA icons (192px, 512px)
  src/
    main.tsx              # App entry, routing, service worker registration
    auth.tsx              # AuthProvider context (session check via /auth/me)
    pages/
      Login.tsx           # OAuth login buttons (Google, Facebook)
      Chat.tsx            # Chat interface with infinite scroll history
    styles.css            # Styling
```
