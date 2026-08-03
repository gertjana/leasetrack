# ─── Stage 1: Build ───────────────────────────────────────────────────────────
# messense/rust-musl-cross provides a native-arm64 Rust toolchain with a
# pre-configured x86_64-unknown-linux-musl cross-compiler. Proc-macros and
# build scripts run natively (no QEMU); only the final binary is cross-compiled.
FROM messense/rust-musl-cross:x86_64-musl AS builder

WORKDIR /build

# Cache dependency compilation separately from source changes.
COPY Cargo.toml ./
COPY core/Cargo.toml ./core/
COPY cli/Cargo.toml  ./cli/
COPY api/Cargo.toml  ./api/

# Create dummy lib/main stubs so `cargo build` can resolve all dependencies.
# CARGO_TARGET_…_RUSTFLAGS scopes -no-pie to the cross-compiled target only,
# so proc-macro crates (which run on the host) are unaffected.
RUN mkdir -p core/src cli/src api/src \
 && echo "pub fn _dummy() {}" > core/src/lib.rs \
 && echo "fn main() {}"       > cli/src/main.rs \
 && echo "fn main() {}"       > api/src/main.rs \
 && CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C link-arg=-no-pie" \
    cargo build --release --target x86_64-unknown-linux-musl --package leasetrack-api \
 && rm -rf core/src cli/src api/src

# Now copy the real source and rebuild only the application code.
COPY core/src ./core/src
COPY cli/src  ./cli/src
COPY api/src  ./api/src

RUN CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C link-arg=-no-pie" \
    cargo build --release --target x86_64-unknown-linux-musl --package leasetrack-api

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

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/leasetrack-api /leasetrack-api

# Run as unprivileged user. scratch has no /etc/passwd, so use the numeric UID
# for nobody (65534) — Docker accepts raw UIDs without a passwd lookup.
USER 65534:65534

EXPOSE 3000

# LEASETRACK_DATA_FILE tells the app where to read/write lease data.
ENV LEASETRACK_DATA_FILE=/data/leasetrack.json

CMD ["/leasetrack-api"]
