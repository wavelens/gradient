# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

"""Scripted stand-in for the Gradient API, serving just what gradient-deploy
reads: the task's newest evaluation, its entry points, and the live WebSocket.

`POST /control/state` swaps the scripted state and pushes one event frame to
every connected socket, which is how the test drives a build to completion.
`GET /control/stats` reports the request and connection counts the test asserts
on to prove the service reacts to events rather than polling.
"""

import base64
import hashlib
import json
import struct
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

STATE = {"evaluation": None, "entry_points": []}
STATS = {"evaluations": 0, "entry_points": 0, "connections": 0}
LOCK = threading.Lock()
SOCKETS = []


def ws_send(sock, opcode, payload):
    header = bytes([0x80 | opcode])
    n = len(payload)
    if n < 126:
        header += bytes([n])
    elif n < 65536:
        header += bytes([126]) + struct.pack(">H", n)
    else:
        header += bytes([127]) + struct.pack(">Q", n)
    sock.sendall(header + payload)


def broadcast(event):
    payload = json.dumps(event).encode()
    with LOCK:
        targets = list(SOCKETS)
    for sock in targets:
        try:
            ws_send(sock, 0x1, payload)
        except OSError:
            with LOCK:
                if sock in SOCKETS:
                    SOCKETS.remove(sock)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("stub: " + fmt % args + "\n")

    def json_response(self, body, code=200):
        raw = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        path = self.path.split("?")[0]

        if path == "/api/v1/health":
            return self.json_response({"error": False, "message": "ok"})

        if path == "/control/stats":
            with LOCK:
                return self.json_response(dict(STATS))

        if path.endswith("/evaluations"):
            with LOCK:
                STATS["evaluations"] += 1
                evaluation = STATE["evaluation"]
            message = [evaluation] if evaluation else []
            return self.json_response({"error": False, "message": message})

        if path.endswith("/entry-points"):
            with LOCK:
                STATS["entry_points"] += 1
                entry_points = list(STATE["entry_points"])
            return self.json_response(
                {"error": False, "message": {"entry_points": entry_points, "total": len(entry_points)}}
            )

        if path.endswith("/live"):
            return self.serve_live()

        self.json_response({"error": True, "message": "not found"}, code=404)

    def do_POST(self):
        if self.path == "/control/reset":
            with LOCK:
                for key in STATS:
                    STATS[key] = 0
            return self.json_response({"error": False, "message": "ok"})

        if self.path != "/control/state":
            return self.json_response({"error": True, "message": "not found"}, code=404)

        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        with LOCK:
            STATE.update(body)
        broadcast({"EvaluationProgress": {"task": None, "evaluation_id": "stub"}})
        self.json_response({"error": False, "message": "ok"})

    def serve_live(self):
        key = self.headers.get("Sec-WebSocket-Key")
        if not key:
            return self.json_response({"error": True, "message": "not a websocket"}, code=400)

        accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
        self.wfile.write(
            (
                "HTTP/1.1 101 Switching Protocols\r\n"
                "Upgrade: websocket\r\n"
                "Connection: Upgrade\r\n"
                f"Sec-WebSocket-Accept: {accept}\r\n\r\n"
            ).encode()
        )
        self.wfile.flush()

        sock = self.connection
        with LOCK:
            STATS["connections"] += 1
            SOCKETS.append(sock)
        try:
            self.pump(sock)
        finally:
            with LOCK:
                if sock in SOCKETS:
                    SOCKETS.remove(sock)
            self.close_connection = True

    def pump(self, sock):
        """Answer the client's pings so the connection survives, until it closes."""
        while True:
            head = self.rfile.read(2)
            if len(head) < 2:
                return
            opcode = head[0] & 0x0F
            masked = head[1] & 0x80
            length = head[1] & 0x7F
            if length == 126:
                length = struct.unpack(">H", self.rfile.read(2))[0]
            elif length == 127:
                length = struct.unpack(">Q", self.rfile.read(8))[0]
            mask = self.rfile.read(4) if masked else b""
            payload = self.rfile.read(length)
            if masked:
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
            if opcode == 0x8:
                return
            if opcode == 0x9:
                ws_send(sock, 0xA, payload)


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
