FROM lukemathwalker/cargo-chef:latest-rust-1.93-bookworm AS chef
WORKDIR /build

FROM chef AS planner
COPY Cargo.toml Cargo.lock image-defaults.toml ./
COPY crates crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock image-defaults.toml ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --bin servarr-operator \
    && cp /build/target/release/servarr-operator /build/servarr-operator

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /build/servarr-operator /servarr-operator
USER nonroot:nonroot
ENTRYPOINT ["/servarr-operator"]
