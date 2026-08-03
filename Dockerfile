# ─── Stage 1: Build ───────────────────────────────────────────────────────────
FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build

# Cache dependency compilation separately from source changes.
COPY Cargo.toml ./
COPY core/Cargo.toml ./core/
COPY cli/Cargo.toml  ./cli/
COPY api/Cargo.toml  ./api/

# Create dummy lib/main stubs so `cargo build` can resolve all dependencies.
# Build scripts and proc-macros run on the host — no special flags needed here.
RUN mkdir -p core/src cli/src api/src \
 && echo "pub fn _dummy() {}" > core/src/lib.rs \
 && echo "fn main() {}"       > cli/src/main.rs \
 && echo "fn main() {}"       > api/src/main.rs \
 && cargo build --release --package leasetrack-api \
 && rm -rf core/src cli/src api/src

# Now copy the real source and rebuild only the application code.
# CARGO_ENCODED_RUSTFLAGS only applies to the compiled crates, not build scripts,
# so proc-macro crates (quote, syn, serde_derive) are unaffected.
COPY core/src ./core/src
COPY cli/src  ./cli/src
COPY api/src  ./api/src

RUN export CARGO_ENCODED_RUSTFLAGS="$(printf -- '-C\x1flink-arg=-no-pie')" \
 && cargo build --release --package leasetrack-api

# ─── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM scratch

# Declare /data as a persistent volume.
VOLUME /data

COPY --from=builder /build/target/release/leasetrack-api /leasetrack-api

USER 65534:65534

EXPOSE 3000

ENV LEASETRACK_DATA_FILE=/data/leasetrack.json

CMD ["/leasetrack-api"]
