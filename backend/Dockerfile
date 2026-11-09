FROM rust:slim AS builder

WORKDIR /usr/src/app
RUN apt-get update && apt-get install -y pkg-config libssl-dev libzstd-dev && rm -rf /var/lib/apt/lists/*

# Compile the whole dependency tree against a dummy main so it lands in the
# layer cache; source-only changes then rebuild just the app crate (~seconds).
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/mbox-browser-backend \
       target/release/deps/mbox_browser_backend*

COPY src ./src
RUN find src -exec touch {} + && cargo build --release --locked

FROM debian:stable-slim
# Final image needs libzstd too
RUN apt-get update && apt-get install -y libssl-dev ca-certificates libzstd1 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/app/target/release/mbox-browser-backend /usr/local/bin/mbox-browser-backend
EXPOSE 3000
CMD ["mbox-browser-backend"]
