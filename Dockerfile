# syntax=docker/dockerfile:1.7
# sds as a container: mount the Docker socket, join the proxy network, done.
FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
# The logo is compiled into the binary for the web UI.
COPY assets/logo/sds-logo.svg ./assets/logo/
# Version is stamped at build time (build.rs reads SDS_VERSION); the release workflow
# passes the tag. Cargo.toml and Cargo.lock remain untouched for reproducible locked builds.
ARG SDS_VERSION=0.0.0-dev
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && cp target/release/sds /sds

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates=* \
    && apt-get clean && rm -rf /var/lib/apt/lists/*
COPY --from=build /sds /usr/local/bin/sds
# The admin API must be reachable from the reverse proxy, so bind to all interfaces inside
# the container and set admin.token (or explicitly use --insecure on a trusted private network).
ENV SDS_CONFIG=/etc/sds/sds.toml
EXPOSE 9900
HEALTHCHECK --interval=30s --timeout=3s CMD ["/usr/local/bin/sds", "status", "--url", "http://127.0.0.1:9900"]
ENTRYPOINT ["/usr/local/bin/sds"]
CMD ["run", "--headless"]
