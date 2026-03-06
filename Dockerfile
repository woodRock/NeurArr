# Build Stage
FROM rust:1.81-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev pkg-config clang
COPY . .
RUN cargo build --release

# Runtime Stage
FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y libssl3 ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/neurarr /app/neurarr
COPY --from=builder /app/migrations /app/migrations

EXPOSE 3000
ENTRYPOINT ["/app/neurarr"]
CMD ["run"]
