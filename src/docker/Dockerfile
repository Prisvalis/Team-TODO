# Build context is the repo root (see docker-compose.yml); only the sources are copied.
FROM rust:1.99-slim-trixie AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# Cache mounts keep the registry and compiled dependencies between builds.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build -p todo-docker --release --locked \
    && cp target/release/todo-docker /usr/local/bin/todo-docker

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home app
COPY --from=build /usr/local/bin/todo-docker /usr/local/bin/todo-docker
ENV TZ=Asia/Taipei
USER app
EXPOSE 8080
CMD ["todo-docker"]
