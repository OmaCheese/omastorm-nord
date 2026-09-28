#!/usr/bin/env python3
"""A local HTTP server for the download bounds (S52): the engine installer
(scripts/test-engine-pin.sh) and the IP-location lookup
(scripts/check-ip-location.sh) must give up on a stalled or oversized reply
without the internet.

  fake-download.py PORTFILE

PORTFILE receives the port once the server listens on 127.0.0.1. Paths:

  /stall            headers never come; the connection is held for 60 s
  /chunked/N        N bytes of "x", chunked, no Content-Length
  /sized/N          N bytes of "x" with a Content-Length
  /padded/N?file=F  file F, then spaces up to N bytes in total, chunked
                    (valid JSON that only a cut at the cap would still parse)
"""
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

CHUNK = 64 * 1024


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def chunked(self, parts):
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        try:
            for part in parts:
                self.wfile.write(b"%x\r\n%s\r\n" % (len(part), part))
            self.wfile.write(b"0\r\n\r\n")
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        url = urlsplit(self.path)
        parts = url.path.strip("/").split("/")
        if parts == ["stall"]:
            time.sleep(60)
            return
        if len(parts) == 2 and parts[1].isdigit():
            size = int(parts[1])
            if parts[0] == "chunked":
                self.chunked(b"x" * min(CHUNK, size - i) for i in range(0, size, CHUNK))
                return
            if parts[0] == "sized":
                self.send_response(200)
                self.send_header("Content-Length", str(size))
                self.end_headers()
                try:
                    for i in range(0, size, CHUNK):
                        self.wfile.write(b"x" * min(CHUNK, size - i))
                except (BrokenPipeError, ConnectionResetError):
                    pass
                return
            if parts[0] == "padded":
                with open(parse_qs(url.query)["file"][0], "rb") as f:
                    body = f.read()
                pad = max(0, size - len(body))
                self.chunked([body] + [b" " * min(CHUNK, pad - i) for i in range(0, pad, CHUNK)])
                return
        self.send_error(404)


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    portfile = sys.argv[1]
    with open(portfile + ".tmp", "w") as f:
        f.write(str(server.server_address[1]))
    os.replace(portfile + ".tmp", portfile)
    server.serve_forever()


if __name__ == "__main__":
    main()
