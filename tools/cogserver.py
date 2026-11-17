"""A local file server with HTTP Range and CORS, for streaming a COG.

Usage: python3 cogserver.py <directory> <port>
"""
import os
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote, urlsplit

ROOT = os.path.abspath(sys.argv[1])
PORT = int(sys.argv[2])
RANGE_RE = re.compile(r"^bytes=(\d*)-(\d*)$")

CORS = {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET, HEAD, OPTIONS",
    "Access-Control-Allow-Headers": "*",
    "Access-Control-Expose-Headers": "Content-Range, Content-Length, Accept-Ranges",
}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _cors(self):
        for k, v in CORS.items():
            self.send_header(k, v)

    def do_OPTIONS(self):
        self.send_response(204)
        self._cors()
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _resolve(self):
        path = unquote(urlsplit(self.path).path)
        full = os.path.abspath(os.path.join(ROOT, path.lstrip("/")))
        if not full.startswith(ROOT + os.sep) and full != ROOT:
            return None
        return full

    def _not_found(self):
        body = b"not found"
        self.send_response(404)
        self._cors()
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def _serve(self, send_body):
        full = self._resolve()
        if full is None or not os.path.isfile(full):
            self._not_found()
            return
        size = os.path.getsize(full)
        rng = self.headers.get("Range")
        start, end = 0, size - 1
        partial = False
        if rng:
            m = RANGE_RE.match(rng.strip())
            if m and size > 0:
                s, e = m.groups()
                if s == "" and e != "":
                    start, end = max(0, size - int(e)), size - 1
                elif s != "":
                    start = int(s)
                    end = int(e) if e else size - 1
                end = min(end, size - 1)
                if start > end or start >= size:
                    self.send_response(416)
                    self._cors()
                    self.send_header("Content-Range", f"bytes */{size}")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                partial = True
        length = end - start + 1 if size else 0
        self.send_response(206 if partial else 200)
        self._cors()
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "image/tiff" if full.lower().endswith(".tif") else "application/octet-stream")
        self.send_header("Content-Length", str(length))
        if partial:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        if not send_body or length == 0:
            return
        with open(full, "rb") as f:
            f.seek(start)
            left = length
            while left > 0:
                chunk = f.read(min(left, 1024 * 1024))
                if not chunk:
                    break
                self.wfile.write(chunk)
                left -= len(chunk)

    def do_GET(self):
        self._serve(True)

    def do_HEAD(self):
        self._serve(False)

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))


if __name__ == "__main__":
    print(f"serving {ROOT} on http://127.0.0.1:{PORT}/", flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
