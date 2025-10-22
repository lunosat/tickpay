# ---- Build stage ----
FROM rust:latest as builder
WORKDIR /app
# Cache deps
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main(){}" > src/main.rs && cargo build --release && rm -rf src
# Build
COPY . .
RUN cargo build --release

# ---- Runtime stage ----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/fake-acquirer /usr/local/bin/fake-acquirer
# Usuário não-root
RUN useradd -ms /bin/bash appuser
USER appuser
ENV RUST_LOG=info \
    ACQ_WEBHOOK_SECRET=change_me \
    PORT=8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/fake-acquirer"]
