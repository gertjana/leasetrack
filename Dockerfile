FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build

COPY . .

RUN cargo build --release --package leasetrack-api

# ─── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM alpine:3.20

RUN apk add --no-cache ca-certificates

VOLUME /data

COPY --from=builder /build/target/release/leasetrack-api /leasetrack-api

EXPOSE 3000

ENV LEASETRACK_DATA_FILE=/data/leasetrack.json
ENV LEASETRACK_USERS_FILE=/data/leasetrack-users.json

CMD ["/leasetrack-api"]
