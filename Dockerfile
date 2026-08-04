FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build

COPY . .

RUN cargo build --release --package leasetrack-api

# ─── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM alpine:latest

VOLUME /data

COPY --from=builder /build/target/release/leasetrack-api /leasetrack-api

EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD wget -qO- http://localhost:3000/health || exit 1

ENV LEASETRACK_DATA_FILE=/data/leasetrack.json

CMD ["/leasetrack-api"]
