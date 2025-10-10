# alpine:3.22.2
FROM alpine@sha256:4b7ce07002c69e8f3d704a9c5d6fd3053be500b7f1c69fc0d80990c2ad8dd412

COPY target/release/trainee-tracker /trainee-tracker

COPY config.prod.json /config.prod.json

CMD ["/trainee-tracker", "/config.prod.json"]
