import argparse
import collections
import csv
import os
import socket
import struct
import threading
import time

import protocol as p


def now_us():
    return time.monotonic_ns() // 1000


def newer(a, b):
    return 0 < (a - b) & 0xFFFFFFFF < 1 << 31


class Reassembler:
    def __init__(self, fps, fec):
        self.lock = threading.Lock()
        self.udp_stale = 2 / fps
        self.fec = fec
        self.frames = {}
        self.latest = None
        self.shown = None
        self.last_udp = time.monotonic()
        self.window = collections.Counter()
        self.skipped = 0
        self.jitter = 0.0
        self.transit = None

    def add(self, packet, udp):
        flags, frame_id, index, total, timestamp, body = p.parse_chunk(packet)
        now = time.monotonic()
        with self.lock:
            if udp:
                self.last_udp = now
            frame = self.frames.get(frame_id)
            if frame is None:
                if self.shown is not None and not newer(frame_id, self.shown):
                    return
                expected = total + (p.parity_groups(total) if udp and self.fec else 0)
                frame = {"expires": now + (self.udp_stale if udp else 1.0), "udp": udp, "data": {}, "parity": {},
                         "received": 0, "expected": expected, "done": False}
                self.frames[frame_id] = frame
                self.track_jitter(timestamp)
            part = frame["parity"] if flags & p.FLAG_PARITY else frame["data"]
            if index in part:
                return
            part[index] = body
            frame["received"] += 1
            data = frame["data"]
            if not frame["done"] and len(data) < total <= len(data) + len(frame["parity"]):
                p.repair(data, frame["parity"], total)
            if not frame["done"] and len(data) == total:
                frame["done"] = True
                self.window["completed"] += 1
                if self.latest is None or newer(frame_id, self.latest[0]):
                    self.skipped += self.latest is not None
                    self.latest = (frame_id, timestamp, b"".join(data[i] for i in range(total)))
                else:
                    self.skipped += 1
            self.expire(now)

    def track_jitter(self, timestamp):
        transit = now_us() - timestamp
        if self.transit is not None:
            self.jitter += (abs(transit - self.transit) - self.jitter) / 16
        self.transit = transit

    def expire(self, now):
        for frame_id in [f for f, v in self.frames.items() if v["expires"] < now]:
            frame = self.frames.pop(frame_id)
            if frame["udp"]:
                self.window["received"] += frame["received"]
                self.window["expected"] += frame["expected"]
            if frame["done"]:
                continue
            if frame["udp"] or self.shown is None or newer(frame_id, self.shown):
                self.window["dropped"] += 1
            else:
                self.window["overtaken"] += 1

    def take_latest(self):
        with self.lock:
            item, self.latest = self.latest, None
            if item:
                self.shown = item[0]
            return item

    def take_window(self):
        with self.lock:
            self.expire(time.monotonic())
            window, self.window = self.window, collections.Counter()
            return window, self.skipped, int(self.jitter)

    def silent_for(self):
        with self.lock:
            return time.monotonic() - self.last_udp

    def grace(self, seconds):
        with self.lock:
            self.last_udp = time.monotonic() + seconds


class Client:
    def __init__(self, args):
        self.args = args
        self.running = True
        self.transport = "?"
        self.send_lock = threading.Lock()
        self.input_lock = threading.Lock()
        self.control = socket.create_connection((args.host, p.CONTROL_PORT), timeout=5)
        self.control.settimeout(None)
        p.set_nodelay(self.control)
        self.udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.udp.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
        self.udp.connect((args.host, p.VIDEO_PORT))
        self.udp.settimeout(0.05)
        self.input_udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.input_udp.connect((args.host, p.INPUT_PORT))
        self.input_tcp = None
        self.token = None
        self.config = None
        self.buffer = None
        self.probe_seq = 0
        self.echoed = set()
        self.samples = collections.deque(maxlen=30)
        self.offset = None
        self.rtt = 0
        self.clean_since = None
        self.events = collections.deque(maxlen=3)
        self.input_seq = 0
        self.displayed = 0
        self.latencies = []
        self.stats = {}
        self.csv = None

    def send(self, kind, payload=b""):
        with self.send_lock:
            self.control.sendall(p.encode_control(kind, payload))

    def expect(self, wanted):
        kind, body = p.read_control(self.control)
        while kind == p.PING:
            seq, t1 = p.PING_BODY.unpack(body)
            self.send(p.PONG, p.PONG_BODY.pack(seq, t1, now_us()))
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
        a = self.args
        self.send(p.CONFIG, p.CONFIG_BODY.pack(a.width, a.height, a.fps, a.quality, a.stripes, a.mode, int(not a.no_fec)))
        self.config = dict(zip(("width", "height", "fps", "quality", "stripes", "mode", "fec"),
                               p.CONFIG_BODY.unpack(self.expect(p.CONFIG_OK))))
        self.buffer = Reassembler(self.config["fps"], self.config["fec"])

    def attach(self, port, kind, stripe):
        sock = socket.create_connection((self.args.host, port), timeout=2)
        sock.settimeout(None)
        p.set_nodelay(sock)
        sock.sendall(p.encode_attach(kind, stripe, self.token))
        return sock

    def stripe_loop(self, stripe):
        try:
            sock = self.attach(p.VIDEO_PORT, 0, stripe)
            while self.running:
                length = struct.unpack(">H", p.read_exact(sock, 2))[0]
                self.buffer.add(p.read_exact(sock, length), False)
        except (OSError, ValueError, struct.error) as error:
            if self.running:
                print(f"stripe {stripe} stopped: {error}")

    def udp_loop(self):
        while self.running:
            try:
                data = self.udp.recv(2048)
            except OSError:
                continue
            if len(data) == p.PROBE.size and data[3] == p.FLAG_PROBE_ECHO:
                self.echoed.add(struct.unpack_from(">I", data, 4)[0])
                continue
            try:
                self.buffer.add(data, True)
            except (ValueError, struct.error):
                pass

    def probe_burst(self, enough):
        seqs = range(self.probe_seq, self.probe_seq + 10)
        self.probe_seq += 10
        for seq in seqs:
            try:
                self.udp.send(p.encode_probe(seq, now_us(), self.token))
            except OSError:
                pass
        deadline = time.monotonic() + 0.3
        while time.monotonic() < deadline and len(self.echoed.intersection(seqs)) < enough:
            time.sleep(0.005)
        return len(self.echoed.intersection(seqs))

    def return_loop(self):
        next_burst = time.monotonic() + 10
        while self.running:
            if self.transport != "TCP" or self.config["mode"] == p.MODE_TCP:
                self.clean_since = None
                next_burst = time.monotonic() + 10
                time.sleep(0.1)
            elif self.clean_since is None:
                if time.monotonic() >= next_burst:
                    next_burst = time.monotonic() + 10
                    if self.probe_burst(8) >= 8:
                        self.clean_since = time.monotonic()
                time.sleep(0.1)
            else:
                started = time.monotonic()
                if self.probe_burst(10) < 9:
                    self.clean_since = None
                time.sleep(max(0, 1 - (time.monotonic() - started)))

    def ping_loop(self):
        seq = 0
        while self.running:
            seq += 1
            try:
                self.send(p.PING, p.PING_BODY.pack(seq, now_us()))
            except OSError:
                return
            time.sleep(1)

    def on_pong(self, body):
        _, t1, t2 = p.PONG_BODY.unpack(body)
        t3 = now_us()
        self.samples.append((t3 - t1, t2 - (t1 + t3) // 2))
        self.rtt, self.offset = min(self.samples)

    def control_loop(self):
        try:
            while self.running:
                kind, body = p.read_control(self.control)
                if kind == p.PING:
                    seq, t1 = p.PING_BODY.unpack(body)
                    self.send(p.PONG, p.PONG_BODY.pack(seq, t1, now_us()))
                elif kind == p.PONG:
                    self.on_pong(body)
                elif kind == p.MIGRATE:
                    self.transport = p.TRANSPORT_NAMES[body[0]]
                    self.clean_since = None
                    if self.transport == "UDP":
                        self.buffer.grace(1.5)
                    print(f"migrated to {self.transport}")
                    with self.input_lock:
                        self.flush_input(list(self.events)[-2:])
                elif kind == p.BYE:
                    if self.running:
                        print(f"host said BYE, reason {body[0]}")
                    break
        except (OSError, ValueError) as error:
            if self.running:
                print(f"control stopped: {error}")
        self.running = False

    def stats_loop(self):
        last = time.monotonic()
        reported_silent = False
        while self.running:
            time.sleep(0.01)
            now = time.monotonic()
            silent = self.transport == "UDP" and self.buffer.silent_for() > 0.5
            if now - last < 0.5 and not (silent and not reported_silent):
                continue
            reported_silent = silent
            try:
                self.send_stats(now - last, silent)
            except OSError:
                return
            last = now

    def send_stats(self, elapsed, silent):
        window, skipped, jitter = self.buffer.take_window()
        closed = window["completed"] + window["dropped"]
        completion = 1000 * window["completed"] // closed if closed else 1000
        expected = window["expected"]
        loss = max(0, 1000 - 1000 * window["received"] // expected) if expected else 0
        shown, self.displayed = self.displayed, 0
        latencies, self.latencies = self.latencies, []
        latency = sum(latencies) // len(latencies) if latencies else 0
        clean = int((time.monotonic() - self.clean_since) * 1000) if self.clean_since else 0
        fps_x10 = round(shown * 10 / elapsed)
        flags = p.STATS_UDP_SILENT if silent else 0
        self.send(p.STATS, p.STATS_BODY.pack(int(elapsed * 1000) & 0xFFFF, loss, completion, jitter & 0xFFFFFFFF,
                                             fps_x10 & 0xFFFF, max(0, latency) & 0xFFFFFFFF, skipped & 0xFFFFFFFF,
                                             clean & 0xFFFFFFFF, flags))
        self.stats = {"time_s": round(time.time(), 3), "transport": self.transport, "window_ms": int(elapsed * 1000),
                      "loss_permille": loss, "completion_permille": completion, "jitter_us": jitter,
                      "fps": fps_x10 / 10, "latency_us": latency, "rtt_us": self.rtt, "skipped": skipped,
                      "overtaken": window["overtaken"], "udp_clean_ms": clean, "udp_silent": int(silent)}
        if self.csv:
            self.csv.writerow(self.stats)

    def send_input(self, kind, state=0, x=0, y=0, code=0):
        with self.input_lock:
            self.input_seq += 1
            self.events.append((self.input_seq, kind, state, x, y, code))
            self.flush_input(list(self.events))

    def flush_input(self, events):
        if not events:
            return
        packet = p.encode_input(self.token, events)
        try:
            if self.transport == "TCP" and self.input_tcp:
                self.input_tcp.sendall(struct.pack(">H", len(packet)) + packet)
            else:
                self.input_udp.send(packet)
        except OSError:
            pass

    def overlay(self):
        s = self.stats
        if not s:
            return [f"{self.transport}"]
        return [f"{s['transport']}  {s['fps']:.1f} fps  done {s['completion_permille'] / 10:.1f}%",
                f"loss {s['loss_permille'] / 10:.1f}%  jitter {s['jitter_us'] / 1000:.1f} ms",
                f"latency {s['latency_us'] / 1000:.1f} ms  rtt {s['rtt_us'] / 1000:.1f} ms"]

    def run(self):
        if self.args.csv:
            out = open(self.args.csv, "w", newline="", buffering=1)
            self.csv = csv.DictWriter(out, ["time_s", "transport", "window_ms", "loss_permille", "completion_permille",
                                            "jitter_us", "fps", "latency_us", "rtt_us", "skipped", "overtaken",
                                            "udp_clean_ms", "udp_silent"])
            self.csv.writeheader()
        self.handshake()
        viewer = None
        if not self.args.headless:
            from viewer import Viewer
            send = None if self.args.no_input else self.send_input
            viewer = Viewer(send, self.config["width"] or 1280, self.config["height"] or 720)
        for stripe in range(self.config["stripes"]):
            threading.Thread(target=self.stripe_loop, args=(stripe,), daemon=True).start()
        try:
            self.input_tcp = self.attach(p.INPUT_PORT, 1, 0)
        except OSError as error:
            print(f"input over TCP unavailable: {error}")
        threading.Thread(target=self.udp_loop, daemon=True).start()
        self.send(p.PROBE_RESULT, bytes([10, self.probe_burst(8)]))
        transport, first_frame = p.START_BODY.unpack(self.expect(p.START))
        self.transport = p.TRANSPORT_NAMES[transport]
        print(f"streaming over {self.transport} from frame {first_frame}, config {self.config}")
        self.buffer.grace(1.5)
        for loop in (self.control_loop, self.stats_loop, self.ping_loop, self.return_loop):
            threading.Thread(target=loop, daemon=True).start()
        try:
            self.display_loop(viewer)
        finally:
            self.running = False
            try:
                self.send(p.BYE, bytes([0]))
            except OSError:
                pass
            for sock in (self.control, self.udp, self.input_udp, self.input_tcp):
                if sock:
                    sock.close()
            print(f"finished, last frame shown {self.buffer.shown}, last stats {self.stats}")

    def display_loop(self, viewer):
        ended_at = time.monotonic() + self.args.seconds if self.args.seconds else None
        try:
            while self.running and (ended_at is None or time.monotonic() < ended_at):
                if viewer and not viewer.pump():
                    break
                item = self.buffer.take_latest()
                if item is None:
                    time.sleep(0.002)
                    continue
                frame_id, timestamp, image = item
                if viewer and not viewer.show(image, self.overlay()):
                    continue
                self.displayed += 1
                if self.offset is not None:
                    self.latencies.append(now_us() + self.offset - timestamp)
        finally:
            if viewer:
                viewer.close()


def main():
    parser = argparse.ArgumentParser(description="Twilight client")
    parser.add_argument("host", nargs="?", help="Omit to open the connect window")
    parser.add_argument("--pin")
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--height", type=int, default=720)
    parser.add_argument("--fps", type=int, default=30)
    parser.add_argument("--quality", type=int, default=60)
    parser.add_argument("--stripes", type=int, default=4)
    parser.add_argument("--mode", type=int, default=p.MODE_AUTO, choices=[0, 1, 2])
    parser.add_argument("--no-fec", action="store_true")
    parser.add_argument("--no-input", action="store_true", help="View only, send no mouse or keyboard input")
    parser.add_argument("--csv", help="Write one row of statistics per STATS window to this file")
    parser.add_argument("--seconds", type=float, default=0)
    parser.add_argument("--headless", action="store_true")
    args = parser.parse_args()
    if args.host:
        if not args.pin:
            parser.error("--pin is required")
        return Client(args).run()
    from launcher import ask
    error = None
    while choice := ask(error):
        vars(args).update(choice)
        try:
            Client(args).run()
            error = None
        except Exception as failure:
            error = f"Could not stream from {args.host}: {failure}"


if __name__ == "__main__":
    main()
