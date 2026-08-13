//go:build tools

package main

// gateway.api is a compatibility scaffold for the existing handlers. The HTTP
// contract is generated from api/openapi.yaml by `ojos service generate`; do not
// regenerate runtime routes from the legacy .api file.
