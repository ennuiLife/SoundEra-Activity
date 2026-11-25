# Build
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p activity-server

# Run: a small image with nothing but the program, as an unprivileged user.
FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin activity \
 && mkdir /data && chown activity /data
COPY --from=build /src/target/release/soundera-activity /usr/local/bin/soundera-activity
USER activity
ENV BIND=0.0.0.0:4540 DATA_DIR=/data
VOLUME /data
EXPOSE 4540
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD ["soundera-activity", "--health"]
ENTRYPOINT ["soundera-activity"]
