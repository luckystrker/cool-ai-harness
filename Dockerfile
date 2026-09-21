# syntax=docker/dockerfile:1.7

# One Rust binary serves the Web API and the production React bundle. Node and
# Rust are build-time only; the runtime image contains neither Python nor Node
# (Bun is never installed — the OpenCode worker is an opt-in sidecar).

FROM node:22-bookworm-slim AS frontend-build
WORKDIR /src/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
# The SPA resolves `@cool-sdk/*` to ../sdk/typescript/src, so the generated SDK
# sources must be present for `tsc -b`.
COPY sdk /src/sdk
RUN npm run build

FROM rust:1.98-bookworm AS core-build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p cool-cli

FROM debian:bookworm-slim AS runtime

ENV COOL_DATA_DIR=/var/lib/cool

# git is required by git-sourced plugins; tini reaps the one entrypoint process.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=core-build /src/target/release/cool /usr/local/bin/cool
COPY --from=frontend-build /src/frontend/dist /opt/cool/frontend/dist

RUN useradd --create-home --uid 10001 cool \
    && mkdir -p /var/lib/cool \
    && chown -R cool:cool /var/lib/cool

USER cool
VOLUME ["/var/lib/cool"]
EXPOSE 8000

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8000"]

ENTRYPOINT ["/usr/bin/tini", "--"]
# Inside the container network namespace the loopback-only default does not
# apply, so the local profile opts into a remote bind. That requires a token:
# pass COOL_API_TOKEN (fail-closed when absent). `--legacy-store` serves the
# legacy families; a fresh volume is initialized at the baseline, while an
# existing Python-owned harness.db stays read-only until `cool store adopt`.
CMD ["cool", "serve", \
     "--data-dir", "/var/lib/cool", \
     "--bind", "0.0.0.0", \
     "--port", "8000", \
     "--assets", "/opt/cool/frontend/dist", \
     "--allow-remote", \
     "--legacy-store"]
