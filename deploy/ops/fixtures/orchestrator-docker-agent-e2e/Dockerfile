# The gate already starts this exact image as its local registry before the
# fixture build. Reusing it avoids a second registry dependency while retaining
# a minimal POSIX userspace for the service and Docker HEALTHCHECK.
FROM registry:2.8.3

ARG OJOS_E2E_VERSION
ENV OJOS_E2E_VERSION=${OJOS_E2E_VERSION}

COPY run.sh /usr/local/bin/ojos-e2e-service
RUN chmod 0555 /usr/local/bin/ojos-e2e-service

HEALTHCHECK --interval=1s --timeout=2s --start-period=1s --retries=10 \
  CMD test -f /tmp/ojos-e2e-healthy || exit 1

ENTRYPOINT ["/usr/local/bin/ojos-e2e-service"]
