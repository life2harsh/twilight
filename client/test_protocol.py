import json
from pathlib import Path

import protocol as p

VECTORS = json.loads((Path(__file__).parent.parent / "spec" / "vectors.json").read_text())


def vector(name):
    return bytes.fromhex(VECTORS["control"][name])


def test_control_framing():
    assert p.encode_control(p.HELLO, bytes.fromhex("544c01")) == vector("HELLO")
    assert p.encode_control(p.KEYFRAME_REQ) == vector("KEYFRAME_REQ")


def test_pair_mac():
    pair = VECTORS["pair"]
    mac = p.pair_mac(pair["pin"], bytes.fromhex(pair["host_nonce"]), bytes.fromhex(pair["client_nonce"]))
    assert mac.hex() == pair["mac"]


def test_config_layout():
    assert p.encode_control(p.CONFIG, p.CONFIG_BODY.pack(1280, 720, 30, 60, 4, 0, 1)) == vector("CONFIG")


def test_stats_layout():
    body = p.STATS_BODY.pack(500, 12, 987, 1500, 298, 21000, 3, 0, 1)
    assert p.encode_control(p.STATS, body) == vector("STATS")


def test_probe_and_attach():
    token = bytes([0xAB] * 16)
    assert p.encode_probe(5, 1_000_000, token).hex() == VECTORS["probe"]
    assert p.encode_attach(0, 2, token).hex() == VECTORS["attach"]


def test_parse_chunk():
    frame = VECTORS["frame"]
    image = b""
    for index, text in enumerate(frame["data"]):
        flags, frame_id, chunk, total, timestamp, body = p.parse_chunk(bytes.fromhex(text))
        assert (frame_id, chunk, total, timestamp) == (258, index, 3, 1_000_000)
        image += body
    assert image == bytes(i % 256 for i in range(2500))


def frame_chunks():
    frame = VECTORS["frame"]
    data = {i: p.parse_chunk(bytes.fromhex(t))[5] for i, t in enumerate(frame["data"])}
    parity = {p.parse_chunk(bytes.fromhex(t))[2]: p.parse_chunk(bytes.fromhex(t))[5] for t in frame["parity"]}
    return data, parity


def test_fec_repairs_any_single_loss():
    full, parity = frame_chunks()
    for lost in full:
        data = {i: b for i, b in full.items() if i != lost}
        p.repair(data, parity, len(full))
        assert data == full


def test_fec_needs_parity():
    full, _ = frame_chunks()
    data = {0: full[0], 1: full[1]}
    p.repair(data, {}, 3)
    assert 2 not in data


def test_input_layout():
    token = bytes([0xAB] * 16)
    events = [(1, p.MOUSE_MOVE, 0, 0x8000, 0x4000, 0), (2, p.MOUSE_BUTTON, 1, 0, 0, 1), (3, p.KEY, 1, 0, 0, 0xFF0D)]
    assert p.encode_input(token, events).hex() == VECTORS["input"]
    assert p.encode_input(token, [(4, p.SCROLL, 0, -1 & 0xFFFF, 3, 0)]).hex() == VECTORS["input_scroll"]
