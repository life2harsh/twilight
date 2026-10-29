import argparse
import os
import socket
import struct
import threading
import time

import cv2
import numpy as np

import protocol as p


def now_us():
    return time.monotonic_ns() // 1000


class Reassembler:
    def __init__(self):
        self.lock = threading.Lock()
        self.frames = {}
        self.latest = None
        self.shown = 0
        self.last_chunk = time.monotonic()
        self.completed = 0
        self.dropped = 0

    def add(self, packet, stale_seconds):
        flags, frame_id, index, total, timestamp, body = p.parse_chunk(packet)
        with self.lock:
            self.last_chunk = time.monotonic()
            if flags & p.FLAG_PARITY or frame_id <= self.shown:
                return
            frame = self.frames.setdefault(frame_id, {"expires": time.monotonic() + stale_seconds, "chunks": {}})
            frame["chunks"][index] = body
            if len(frame["chunks"]) == total:
                del self.frames[frame_id]
                self.completed += 1
                image = b"".join(frame["chunks"][i] for i in range(total))
                self.latest = (frame_id, timestamp, image)
            self.drop_stale()

    def drop_stale(self):
        now = time.monotonic()
        for frame_id in [f for f, v in self.frames.items() if v["expires"] < now]:
            del self.frames[frame_id]
            self.dropped += 1

    def take_latest(self):
        with self.lock:
            item, self.latest = self.latest, None
            if item:
                self.shown = item[0]
            return item

    def take_counts(self):
        with self.lock:
            counts = self.completed, self.dropped
            self.completed = self.dropped = 0
            return counts

    def silent_for(self):
        with self.lock:
            return time.monotonic() - self.last_chunk


class Client:
    def __init__(self, args):
        self.args = args
        self.running = True
        self.transport = "?"
        self.buffer = Reassembler()
        self.send_lock = threading.Lock()
        self.control = socket.create_connection((args.host, p.CONTROL_PORT), timeout=5)
        self.control.settimeout(None)
        p.set_nodelay(self.control)
        self.udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.udp.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
        self.udp.connect((args.host, p.VIDEO_PORT))
        self.udp.settimeout(0.05)
        self.token = None
        self.displayed = 0

    def send(self, kind, payload=b""):
        with self.send_lock:
            self.control.sendall(p.encode_control(kind, payload))

    def expect(self, wanted):
        kind, body = p.read_control(self.control)
        if kind == p.BYE:
            raise ConnectionError(f"host said BYE, reason {body[0]}")
        if kind == p.PAIR_FAIL:
            raise PermissionError(f"wrong PIN, {body[0]} attempts left")
        if kind != wanted:
            raise ValueError(f"expected message {wanted}, got {kind}")
        return body

    def handshake(self):
        self.send(p.HELLO, struct.pack(">HB", p.MAGIC, p.VERSION))
        host_nonce = self.expect(p.HELLO_OK)[1:]
        client_nonce = os.urandom(16)
        self.send(p.PAIR, client_nonce + p.pair_mac(self.args.pin, host_nonce, client_nonce))
        self.token = self.expect(p.PAIR_OK)
        width, height = self.args.width, self.args.height
        config = p.CONFIG_BODY.pack(width, height, self.args.fps, self.args.quality, self.args.stripes, self.args.mode, 1)
        self.send(p.CONFIG, config)
        self.expect(p.CONFIG_OK)

    def attach_stripes(self):
        for stripe in range(self.args.stripes):
            threading.Thread(target=self.stripe_loop, args=(stripe,), daemon=True).start()

    def stripe_loop(self, stripe):
        try:
            sock = socket.create_connection((self.args.host, p.VIDEO_PORT), timeout=2)
            sock.settimeout(None)
            p.set_nodelay(sock)
            sock.sendall(p.encode_attach(0, stripe, self.token))
            while self.running:
                length = struct.unpack(">H", p.read_exact(sock, 2))[0]
                self.buffer.add(p.read_exact(sock, length), 1.0)
        except OSError as error:
            print(f"stripe {stripe} stopped: {error}")

