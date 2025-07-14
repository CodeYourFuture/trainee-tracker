FROM ekidd/rust-musl-builder@sha256:c18dbd9fcf3a4c0c66b8aacea5cf977ee38193efd7e98a55ee7bf9cd9954b221 AS build

RUN sudo chown -R rust:rust /opt/rust/rustup

RUN rustup install 1.88.0

RUN rustup +1.88.0 target add x86_64-unknown-linux-musl

RUN mkdir /tmp/src && chown rust:rust /tmp/src

COPY . /tmp/src

RUN cargo +1.88.0 build --release --manifest-path=/tmp/src/Cargo.toml

FROM ubuntu:latest

COPY --from=build /tmp/src/target/x86_64-unknown-linux-musl/release/trainee-tracker /trainee-tracker

COPY config.prod.json /config.prod.json

CMD ["/trainee-tracker", "/config.prod.json"]
