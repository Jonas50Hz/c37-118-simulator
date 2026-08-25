FROM rust:1.85-bookworm AS build

WORKDIR /build

COPY Cargo.toml Cargo.lock /build/
COPY src /build/src
COPY profiles /build/profiles
COPY scenarios /build/scenarios

RUN cargo build --release --locked --bins

FROM build AS test

RUN cargo test --locked

FROM debian:bookworm-slim AS runtime

RUN useradd --system --uid 10001 --create-home simulator \
	&& mkdir -p /run/c37-118 \
	&& chown simulator:simulator /run/c37-118

COPY --from=build /build/target/release/c37-118-simulator /usr/local/bin/c37-118-simulator
COPY --from=build /build/target/release/c37-118-probe /usr/local/bin/c37-118-probe
COPY profiles /etc/c37-118/profiles
COPY scenarios /etc/c37-118/scenarios

USER simulator

ENTRYPOINT ["/usr/local/bin/c37-118-simulator"]