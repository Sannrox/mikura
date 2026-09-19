ARG BASE_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

FROM ${BASE_IMAGE}

ARG VCS_REF=unknown
LABEL org.opencontainers.image.revision=$VCS_REF

RUN useradd --system --create-home mikura \
    && mkdir /data && chown mikura:mikura /data

COPY mikura-host /usr/local/bin/

VOLUME /data

USER mikura
ENTRYPOINT ["mikura-host"]
