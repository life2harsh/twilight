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

    def probe(self):
        for seq in range(10):
            try:
                self.udp.send(p.encode_probe(seq, now_us(), self.token))
            except OSError:
                pass
        echoed = 0
        deadline = time.monotonic() + 0.3
        while echoed < 8 and time.monotonic() < deadline:
            try:
                data = self.udp.recv(2048)
            except OSError:
                continue
            if len(data) == p.PROBE.size and data[3] == p.FLAG_PROBE_ECHO:
                echoed += 1
        self.send(p.PROBE_RESULT, bytes([10, echoed]))

    def udp_loop(self):
        while self.running:
            try:
                data = self.udp.recv(2048)
            except OSError:
                continue
            if data[3] & p.FLAG_PROBE:
                continue
            try:
                self.buffer.add(data, 2 / self.args.fps)
            except (ValueError, struct.error):
                pass

    def control_loop(self):
        try:
            while self.running:
                kind, body = p.read_control(self.control)
                if kind == p.PING:
                    seq, t1 = p.PING_BODY.unpack(body)
                    self.send(p.PONG, p.PONG_BODY.pack(seq, t1, now_us()))
                elif kind == p.MIGRATE:
                    self.transport = p.TRANSPORT_NAMES[body[0]]
                    print(f"migrated to {self.transport}")
                elif kind == p.BYE:
                    print(f"host said BYE, reason {body[0]}")
                    break
        except (OSError, ValueError) as error:
            print(f"control stopped: {error}")
        self.running = False

    def stats_loop(self):
        while self.running:
            time.sleep(0.5)
            completed, dropped = self.buffer.take_counts()
            closed = completed + dropped
            completion = 1000 * completed // closed if closed else 1000
            shown, self.displayed = self.displayed, 0
            silent = self.transport == "UDP" and self.buffer.silent_for() > 0.5
            flags = p.STATS_UDP_SILENT if silent else 0
            body = p.STATS_BODY.pack(500, 0, completion, 0, shown * 20, 0, 0, 0, flags)
            try:
                self.send(p.STATS, body)
            except OSError:
                return

    def run(self):
        self.handshake()
        self.attach_stripes()
        self.probe()
        transport, first_frame = p.START_BODY.unpack(self.expect(p.START))
        self.transport = p.TRANSPORT_NAMES[transport]
        print(f"streaming over {self.transport} from frame {first_frame}")
        self.buffer.last_chunk = time.monotonic() + 1.5
        for loop in (self.control_loop, self.udp_loop, self.stats_loop):
            threading.Thread(target=loop, daemon=True).start()
        self.display_loop()

    def display_loop(self):
        ended_at = time.monotonic() + self.args.seconds if self.args.seconds else None
        while self.running and (ended_at is None or time.monotonic() < ended_at):
            item = self.buffer.take_latest()
            if item is None:
                time.sleep(0.002)
                continue
            frame_id, _, image = item
            self.displayed += 1
            if self.args.headless:
                continue
            frame = cv2.imdecode(np.frombuffer(image, np.uint8), cv2.IMREAD_COLOR)
            if frame is None:
                continue
            cv2.putText(frame, f"{self.transport} frame {frame_id}", (10, 30), cv2.FONT_HERSHEY_SIMPLEX, 0.8, (0, 255, 0), 2)
            cv2.imshow("Twilight", frame)
            if cv2.waitKey(1) & 0xFF == ord("q"):
                break
        self.running = False
        print(f"finished, last frame shown {self.buffer.shown}")
        try:
            self.send(p.BYE, bytes([0]))
        except OSError:
            pass


def main():
    parser = argparse.ArgumentParser(description="Twilight client")
    parser.add_argument("host")
    parser.add_argument("--pin", required=True)
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--height", type=int, default=720)
    parser.add_argument("--fps", type=int, default=30)
    parser.add_argument("--quality", type=int, default=60)
    parser.add_argument("--stripes", type=int, default=4)
    parser.add_argument("--mode", type=int, default=p.MODE_AUTO, choices=[0, 1, 2])
    parser.add_argument("--seconds", type=float, default=0)
    parser.add_argument("--headless", action="store_true")
    Client(parser.parse_args()).run()


if __name__ == "__main__":
    main()
