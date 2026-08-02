# ─── Stage 1: Build ───────────────────────────────────────────────────────────
# rust:alpine uses musl by default, producing fully static binaries.
FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build

# Cache dependency compilation separately from source changes.
COPY Cargo.toml ./
COPY core/Cargo.toml ./core/
COPY cli/Cargo.toml  ./cli/
COPY api/Cargo.toml  ./api/

# Create dummy lib/main stubs so `cargo build` can resolve all dependencies.
RUN mkdir -p core/src cli/src api/src \
 && echo "pub fn _dummy() {}" > core/src/lib.rs \
 && echo "fn main() {}"       > cli/src/main.rs \
 && echo "fn main() {}"       > api/src/main.rs \
 && cargo build --release --package leasetrack-api \
 && rm -rf core/src cli/src api/src

# Now copy the real source and rebuild only the application code.
COPY core/src ./core/src
COPY cli/src  ./cli/src
COPY api/src  ./api/src

# rust:alpine uses x86_64-unknown-linux-musl which defaults to static linking —
# no RUSTFLAGS needed. Adding -C target-feature=+crt-static explicitly breaks
# proc-macro crates (serde_derive etc.) that must be compiled as dylibs.
RUN cargo build --release --package leasetrack-api

# ─── Stage 2: Runtime ─────────────────────────────────────────────────────────
# mosakram/ark-os is a minimal container-native OS (~2 MB).
# It requires fully static binaries — no shared libraries.
FROM mosakram/ark-os

# Declare /data as a persistent volume.
# Docker will always create a volume here — use a named volume so data
# survives container replacements:
#   docker run -v leasetrack-data:/data leasetrack-api
# Or with compose: volumes: [leasetrack-data:/data]
VOLUME /data

COPY --from=builder /build/target/release/leasetrack-api /leasetrack-api

# Run as unprivileged user when the image provides one.
# ark-os includes a `nobody` user via BusyBox.
USER nobody

EXPOSE 3000

# LEASETRACK_DATA_FILE tells the app where to read/write lease data.
ENV LEASETRACK_DATA_FILE=/data/leasetrack.json

CMD ["/leasetrack-api"]
