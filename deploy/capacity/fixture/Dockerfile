ARG CAPACITY_FIXTURE_BASE_IMAGE=docker.io/library/alpine@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce
FROM ${CAPACITY_FIXTURE_BASE_IMAGE}

ARG GITHUB_SHA
LABEL org.opencontainers.image.revision="$GITHUB_SHA"

COPY run.sh /usr/local/bin/ojos-capacity-fixture
COPY http-handler.sh /usr/local/bin/ojos-capacity-http-handler
RUN chmod 0555 /usr/local/bin/ojos-capacity-fixture /usr/local/bin/ojos-capacity-http-handler

HEALTHCHECK --interval=5s --timeout=2s --start-period=2s --retries=12 \
  CMD wget -q -T 1 -O - http://127.0.0.1:8080/health | grep -q '"status":"healthy"' || exit 1

EXPOSE 8080/tcp

ENTRYPOINT ["/usr/local/bin/ojos-capacity-fixture"]
