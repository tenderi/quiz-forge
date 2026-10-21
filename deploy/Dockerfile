# Runtime image for the static musl binary built by .github/workflows/release.yml
# (or `cargo build --release --target x86_64-unknown-linux-musl`).
#   docker build -f deploy/Dockerfile -t quiz-forge .
FROM alpine:3.22
RUN apk add --no-cache ca-certificates tzdata && adduser -D -u 10001 quizforge
COPY target/x86_64-unknown-linux-musl/release/quiz-forge /usr/local/bin/quiz-forge
USER quizforge
WORKDIR /data
ENV DATA_DIR=/data LISTEN=0.0.0.0:8090
EXPOSE 8090
VOLUME ["/data"]
ENTRYPOINT ["quiz-forge"]
CMD ["serve"]
