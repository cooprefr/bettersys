FROM rust:1.83-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./

# Cache dependencies
RUN mkdir -p src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/betterbot /usr/local/bin/betterbot

EXPOSE 3000

ENV RUST_LOG=info

CMD ["betterbot"]
