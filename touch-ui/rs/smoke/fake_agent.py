#!/usr/bin/env python3
"""A fake u60d screen API for rs/smoke.sh on 127.0.0.1:9090 (fake_core.py forwards /v2/api/... here): /api/public/status (the
scenario 在家, no unread alerts, the device check fine); everything else 404. Requests are logged
to the file given as the first argument.
SPDX-License-Identifier: MIT
"""
import http.server
import json
import sys

LOG = open(sys.argv[1], "a", buffering=1)
STATUS = {"ok": True, "data": {
    "alerts": {"unread": 0},
    "health": {"checked": True, "bad": 0, "warn": 0},
    "scenario": {"configured": True, "enabled": True, "name": "在家", "name_en": "Home", "wifi_off": False,
                 "pin": None, "last_switch": 0, "abroad": False},
}}


class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, code, body):
        b = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        LOG.write(json.dumps({"req": "GET " + self.path}) + "\n")
        if self.path == "/api/public/status":
            self.reply(200, STATUS)
        else:
            self.reply(404, {"ok": False})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(n)
        LOG.write(json.dumps({"req": "POST " + self.path}) + "\n")
        self.reply(404, {"ok": False})

    do_PUT = do_POST


http.server.ThreadingHTTPServer(("127.0.0.1", 9090), H).serve_forever()
