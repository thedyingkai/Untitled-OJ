#!/usr/bin/env python3
import argparse
import http.server
import json
import pathlib
import tempfile


PERMISSION_PATH = "/auth/admin/permission-check"


def write_json_atomically(path: pathlib.Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=path.parent,
        prefix=f".{path.name}.",
        delete=False,
    ) as pending:
        json.dump(value, pending, separators=(",", ":"), sort_keys=True)
        pending.write("\n")
    pathlib.Path(pending.name).replace(path)


def build_handler(token: str, evidence_file: pathlib.Path):
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self) -> None:
            if self.path != PERMISSION_PATH:
                self.send_error(404)
                return
            if self.headers.get("Authorization") != f"Bearer {token}":
                self.send_error(401)
                return
            try:
                size = int(self.headers.get("Content-Length", "0"))
                payload = json.loads(self.rfile.read(size))
            except (ValueError, json.JSONDecodeError):
                self.send_error(400)
                return

            allowed = (
                payload.get("user_id") == 1
                and payload.get("permission") == "judge.admin"
                and payload.get("scope_type") == "system"
                and payload.get("scope_id", 0) == 0
            )
            write_json_atomically(
                evidence_file,
                {
                    "method": "POST",
                    "path": self.path,
                    "authorization_verified": True,
                    "request": payload,
                    "decision": "allowed" if allowed else "denied",
                },
            )
            body = json.dumps(
                {"code": 0, "msg": "success", "data": {"allowed": allowed}},
                separators=(",", ":"),
            ).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, fmt: str, *args: object) -> None:
            print(fmt % args, flush=True)

    return Handler


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--token", required=True)
    parser.add_argument("--ready-file", type=pathlib.Path, required=True)
    parser.add_argument("--evidence-file", type=pathlib.Path, required=True)
    args = parser.parse_args()

    server = http.server.ThreadingHTTPServer(
        (args.host, args.port), build_handler(args.token, args.evidence_file)
    )
    write_json_atomically(
        args.ready_file,
        {"ready": True, "host": args.host, "port": server.server_address[1]},
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
