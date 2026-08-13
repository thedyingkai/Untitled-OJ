#!/usr/bin/env python3
"""Small TLS-only static server for the signed frontend artifact E2E fixture."""

import argparse
import http.server
import ssl


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--port", required=True, type=int)
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    args = parser.parse_args()

    handler = lambda *values, **kwargs: http.server.SimpleHTTPRequestHandler(  # noqa: E731
        *values, directory=args.root, **kwargs
    )
    server = http.server.ThreadingHTTPServer(("0.0.0.0", args.port), handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.cert, args.key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
