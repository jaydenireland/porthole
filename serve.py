#!/usr/bin/env python3
"""Serves web/ on localhost for development, with browser caching turned off.

    ./serve.py [port]    (default 8791)

Python's plain http.server sends no caching headers, so browsers keep using
old copies of scripts after you edit them. This adds "Cache-Control: no-store".
"""
import functools
import http.server
import os
import sys


class Handler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


port = int(sys.argv[1]) if len(sys.argv) > 1 else 8791
web = os.path.join(os.path.dirname(os.path.abspath(__file__)), "web")
print(f"Serving {web} at http://localhost:{port}")
http.server.ThreadingHTTPServer(("127.0.0.1", port), functools.partial(Handler, directory=web)).serve_forever()
