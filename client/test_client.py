import time

import protocol as p
from client import Reassembler, newer
from test_protocol import VECTORS


def chunk(frame_id, index, total, body=b"x", flags=0):
    return p.CHUNK_HEADER.pack(p.MAGIC, p.VERSION, flags, frame_id, index, total, 5) + body


def vector_packets():
    return [bytes.fromhex(t) for t in VECTORS["frame"]["data"]], [bytes.fromhex(t) for t in VECTORS["frame"]["parity"]]


def close_all(buffer):
    for frame in buffer.frames.values():
        frame["expires"] = 0
    return buffer.take_window()[0]


def test_fec_completes_frame_and_counts_loss():
    data, parity = vector_packets()
    buffer = Reassembler(30, True)
    for packet in data[1:] + parity:
        buffer.add(packet, True)
    frame_id, _, image = buffer.take_latest()
    assert frame_id == 258 and image == bytes(i % 256 for i in range(2500))
    window = close_all(buffer)
    assert (window["completed"], window["received"], window["expected"]) == (1, 3, 4)


def test_stale_udp_frame_is_dropped():
    buffer = Reassembler(30, False)
    buffer.add(chunk(1, 0, 2), True)
    time.sleep(0.07)
    buffer.add(chunk(2, 0, 1), True)
    window = buffer.take_window()[0]
    assert window["dropped"] == 1 and window["completed"] == 1


def test_overtaken_tcp_frame_is_not_a_failure():
    buffer = Reassembler(30, True)
    buffer.add(chunk(1, 0, 2), False)
    buffer.add(chunk(2, 0, 1), False)
    buffer.take_latest()
    window = close_all(buffer)
    assert (window["dropped"], window["overtaken"], window["expected"]) == (0, 1, 0)


def test_old_frames_ignored_and_skips_counted():
    buffer = Reassembler(30, False)
    buffer.add(chunk(5, 0, 1), False)
    buffer.add(chunk(6, 0, 1), False)
    assert buffer.take_latest()[0] == 6 and buffer.skipped == 1
    buffer.add(chunk(4, 0, 1), False)
    assert buffer.take_latest() is None


def test_serial_wrap():
    assert newer(0, 0xFFFFFFFF) and not newer(0xFFFFFFFF, 0) and not newer(3, 3)
