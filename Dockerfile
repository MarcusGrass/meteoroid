FROM rust:1-trixie

ARG USERID
ARG GROUPID

RUN mkdir -p /data/workdir && mkdir -p /data/local && mkdir -p /data/upstream && mkdir -p /data/output && rustup component add rustfmt && rustup +stable component add rustfmt
WORKDIR /app
RUN chown -R $USERID:$GROUPID /app
USER $USERID


COPY . .

ENTRYPOINT ["cargo", "r", "-r", "-p", "meteoroid", "--", "-o", "/data/output", "-w", "/data/workdir", "--rustfmt-local-repo", "/data/local", "--rustfmt-upstream-repo", "/data/upstream"]