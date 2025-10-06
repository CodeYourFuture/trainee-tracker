FROM alpine:latest

COPY target/release/trainee-tracker /trainee-tracker

COPY config.prod.json /config.prod.json

CMD ["/trainee-tracker", "/config.prod.json"]
