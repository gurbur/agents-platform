#!/usr/bin/env python3
"""Throwaway receiver for deploy/smoke.sh --receiver-url.

Answers every POST with an empty 202 (Content-Length: 0), the only success
RECEIVER_WAKE_CONVENTION allows, and logs one line per notification: the
JSON body ({v, agent_id, inbox_seq}) and whether a Bearer header was present.
The secret itself is never printed. Not a receiver implementation: it does
not check the secret and does nothing with the notification.

Usage: python3 -I deploy/mock-receiver.py <bind-address> <port>
  e.g. python3 -I deploy/mock-receiver.py 100.x.y.z 8799   (Mac's Tailscale IP)
  then: deploy/smoke.sh ... --receiver-url http://100.x.y.z:8799/notify
"""

import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MAX_BODY_BYTES = 1024
USAGE = "usage: python3 -I deploy/mock-receiver.py <bind-address> <port>"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(min(length, MAX_BODY_BYTES))
        bearer = self.headers.get("Authorization", "").startswith("Bearer ")
        print(
            f"notify {self.path} bearer={'yes' if bearer else 'no'} "
            f"body={body.decode('utf-8', 'replace')}",
            flush=True,
        )
        self.send_response(202)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, format, *args):
        # One line per notification is printed by do_POST; skip the access log.
        pass


def main():
    if len(sys.argv) != 3:
        print(USAGE, file=sys.stderr)
        sys.exit(2)
    address, port = sys.argv[1], int(sys.argv[2])
    server = ThreadingHTTPServer((address, port), Handler)
    print(f"mock receiver listening on http://{address}:{port}/", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
