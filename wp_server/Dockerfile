FROM rust:1.73-alpine

COPY ./* /root/
WORKDIR /root

RUN cargo build

# RUN apk add --no-cache tor