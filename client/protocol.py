import hashlib
import hmac
import socket
import struct

MAGIC = 0x544C
VERSION = 1
CONTROL_PORT = 48100
VIDEO_PORT = 48101
INPUT_PORT = 48102

HELLO = 1
HELLO_OK = 2
PAIR = 3
PAIR_OK = 4
PAIR_FAIL = 5
CONFIG = 6
CONFIG_OK = 7
PROBE_RESULT = 8
START = 9
STATS = 10
MIGRATE = 11
PING = 12
PONG = 13
KEYFRAME_REQ = 14
BYE = 15

MODE_AUTO = 0
MODE_UDP = 1
MODE_TCP = 2
TRANSPORT_NAMES = {0: "UDP", 1: "TCP"}

FLAG_PARITY = 0x02
FLAG_PROBE = 0x80
FLAG_PROBE_ECHO = 0x81
STATS_UDP_SILENT = 0x01

CHUNK_HEADER = struct.Struct(">HBBIHHQ")
CONTROL_HEADER = struct.Struct(">IB")
CONFIG_BODY = struct.Struct(">HHBBBBB")
PROBE = struct.Struct(">HBBIQ16s")
ATTACH = struct.Struct(">HBBBB16s")
STATS_BODY = struct.Struct(">HHHIHIIIB")
PING_BODY = struct.Struct(">IQ")
PONG_BODY = struct.Struct(">IQQ")
START_BODY = struct.Struct(">BI")


def encode_control(kind, payload=b""):
    return CONTROL_HEADER.pack(len(payload), kind) + payload


def read_exact(sock, size):
    data = bytearray()
    while len(data) < size:
        part = sock.recv(size - len(data))
        if not part:
            raise ConnectionError("peer closed the connection")
        data += part
    return bytes(data)


def read_control(sock):
    length, kind = CONTROL_HEADER.unpack(read_exact(sock, CONTROL_HEADER.size))
    if length > 1024:
        raise ValueError("control message too long")
    return kind, read_exact(sock, length)


def pair_mac(pin, host_nonce, client_nonce):
    message = b"twilight-pair-v1" + host_nonce + client_nonce
    return hmac.new(pin.encode(), message, hashlib.sha256).digest()


def encode_probe(seq, time_us, token):
    return PROBE.pack(MAGIC, VERSION, FLAG_PROBE, seq, time_us, token)


def encode_attach(kind, stripe, token):
    return ATTACH.pack(MAGIC, VERSION, kind, stripe, 0, token)


def parse_chunk(data):
    magic, version, flags, frame_id, index, total, timestamp = CHUNK_HEADER.unpack_from(data)
    if magic != MAGIC or version != VERSION:
        raise ValueError("not a Twilight packet")
    return flags, frame_id, index, total, timestamp, data[CHUNK_HEADER.size:]


def set_nodelay(sock):
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
