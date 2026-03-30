# Stage 1: Build the client
FROM node:22-slim AS client-builder
WORKDIR /app/client
COPY client/package.json client/package-lock.json ./
RUN npm ci
COPY client/ ./
RUN npm run build

# Stage 2: Build the server
FROM rust:slim-bookworm AS server-builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev protobuf-compiler libopencc-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY server/Cargo.toml server/Cargo.lock ./server/
# Create dummy main to cache dependency build
RUN mkdir -p server/src && echo "fn main() {}" > server/src/main.rs && \
    cd server && cargo build --release && rm -rf src
# Copy real source and client dist
COPY server/src/ ./server/src/
COPY --from=client-builder /app/client/dist/ ./client/dist/
# Touch main.rs so cargo rebuilds it (not the cached dummy)
RUN touch server/src/main.rs && cd server && cargo build --release

# Stage 3: Runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates libopencc1.1 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=server-builder /app/server/target/release/sidekick-server .
COPY server/config.toml .
RUN mkdir -p data/users
EXPOSE 3000
CMD ["./sidekick-server"]
