# ─── Stage 1: Build ───────────────────────────────────────────────────────────
# rust:alpine on linux/amd64 (CI runner) builds natively.
# On Apple Silicon, build with: docker build --platform linux/amd64
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

RUN cargo build --release --package leasetrack-api

# ─── Stage 2: Runtime ─────────────────────────────────────────────────────────
# scratch is an empty image (~0 MB). The binary is statically linked so no
# shared libraries are needed.
FROM scratch

# Declare /data as a persistent volume.
# Docker will always create a volume here — use a named volume so data
# survives container replacements:
#   docker run -v leasetrack-data:/data leasetrack-api
# Or with compose: volumes: [leasetrack-data:/data]
VOLUME /data

COPY --from=builder /build/target/release/leasetrack-api /leasetrack-api

# Run as unprivileged user. scratch has no /etc/passwd, so use the numeric UID
# for nobody (65534) — Docker accepts raw UIDs without a passwd lookup.
USER 65534:65534

EXPOSE 3000

# LEASETRACK_DATA_FILE tells the app where to read/write lease data.
ENV LEASETRACK_DATA_FILE=/data/leasetrack.json

CMD ["/leasetrack-api"]
