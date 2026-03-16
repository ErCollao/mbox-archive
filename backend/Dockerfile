FROM rust:slim AS builder

WORKDIR /usr/src/app
# Added git to fetch dependencies from github
RUN apt-get update && apt-get install -y pkg-config libssl-dev libzstd-dev git && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release

FROM debian:stable-slim
# Final image needs libzstd too
RUN apt-get update && apt-get install -y libssl-dev ca-certificates libzstd1 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/app/target/release/mbox-browser-backend /usr/local/bin/mbox-browser-backend
EXPOSE 3000
CMD ["mbox-browser-backend"]
