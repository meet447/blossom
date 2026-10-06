#!/usr/bin/env python3
"""Inject one tablet point, then `echo hi`, `ls`, `cat note`, and `write hi there`."""

import json
import socket
import sys
import time

PIXEL = 128
AXIS_MAX = 32767
# Down then up. Space is `spc` and enter is `ret` in QEMU's qcode list.
KEYS = ("e", "c", "h", "o", "spc", "h", "i", "ret")
# A second batch. One batch of every key would overflow the 32-event queue.
DIRECTORY = ("l", "s", "ret")
CAT = (
    "c",
    "a",
    "t",
    "spc",
    "slash",
    "h",
    "o",
    "m",
    "e",
    "slash",
    "n",
    "o",
    "t",
    "e",
    "ret",
)
# Fifteen key-downs is thirty events, which still fits the queue of 32.
WRITE = (
    "w",
    "r",
    "i",
    "t",
    "e",
    "spc",
    "h",
    "i",
    "spc",
    "t",
    "h",
    "e",
    "r",
    "e",
    "ret",
)


def main() -> None:
    log_path = sys.argv[1]
    sock_path = sys.argv[2]
    width, height = wait_for(log_path, 80, listening)
    stream = connect(sock_path)
    read_msg(stream)
    write_msg(stream, {"execute": "qmp_capabilities"})
    read_until_return(stream)
    send_pointer(stream, width, height)
    wait_for(log_path, 40, terminal_ready)
    send_keys(stream, KEYS, "echo hi")
    wait_for(log_path, 40, line_ready)
    send_keys(stream, DIRECTORY, "ls")
    wait_for(log_path, 40, listing_ready)
    send_keys(stream, CAT, "cat /home/note")
    wait_for(log_path, 40, directory_ready)
    send_keys(stream, WRITE, "write hi there")
    wait_for(log_path, 40, write_ready)
    for command, marker in FS_STEPS:
        send_keys(stream, qcodes(command), command)
        wait_for(log_path, 40, lambda text, marker=marker: True if marker in text else None)
    send_keys(stream, qcodes("run /bin/hello"), "run /bin/hello")
    wait_for(log_path, 60, hello_ready)
    send_keys(stream, qcodes("run /bin/fault"), "run /bin/fault")
    wait_for(log_path, 60, fault_ready)
    send_keys(stream, qcodes("ping 10.0.2.2"), "ping 10.0.2.2")
    wait_for(log_path, 60, ping_ready)
    send_keys(stream, qcodes("fetch 10.0.2.100"), "fetch 10.0.2.100")
    wait_for(log_path, 60, fetch_ready)


def listening(text: str):
    width = None
    height = None
    for line in text.splitlines():
        marker = "meuxe: desktop fb "
        if marker in line:
            spec = line.split(marker, 1)[1].strip()
            wide, _, high = spec.partition("x")
            if wide.isdigit() and high.isdigit():
                width = int(wide)
                height = int(high)
    if width and height and "meuxe: tablet listening" in text:
        return (width, height)
    return None


def terminal_ready(text: str):
    if "meuxe: desktop ready" in text and "meuxe: kbd listening" in text:
        return True
    return None


def line_ready(text: str):
    if "meuxe: shell line=hi" in text:
        return True
    return None


def listing_ready(text: str):
    if "meuxe: shell ls=/ bin etc home tmp" in text:
        return True
    return None


def directory_ready(text: str):
    if "meuxe: directory ready" in text:
        return True
    return None


def write_ready(text: str):
    if "meuxe: write ready" in text:
        return True
    return None


def hello_ready(text: str):
    if "meuxe: shell run=/bin/hello exit=0" in text:
        return True
    return None


def fault_ready(text: str):
    if "meuxe: shell run=/bin/fault exit=fault" in text:
        return True
    return None


def ping_ready(text: str):
    if "meuxe: shell ping=10.0.2.2 rx=4/4" in text:
        return True
    return None


def fetch_ready(text: str):
    if "meuxe: shell fetch=10.0.2.100 status=200 bytes=11 body=meuxe-alpha" in text:
        return True
    return None


FS_STEPS = (
    ("mkdir /tmp/d", "meuxe: shell mkdir=/tmp/d ok"),
    ("write /tmp/d/a one", "meuxe: shell write=/tmp/d/a ok"),
    ("append /tmp/d/a two", "meuxe: shell append=/tmp/d/a ok"),
    ("cp /tmp/d/a /tmp/d/b", "meuxe: shell cp=/tmp/d/b ok"),
    ("mv /tmp/d/b /home/b", "meuxe: shell mv=/home/b ok"),
    ("cat /home/b", "meuxe: shell cat=/home/b onetwo"),
    ("rm /tmp/d/a", "meuxe: shell rm=/tmp/d/a ok"),
    ("rm /tmp/d", "meuxe: shell rm=/tmp/d ok"),
    ("df", "meuxe: shell df free="),
)


def qcodes(text: str):
    keys = []
    for ch in text:
        if ch == " ":
            keys.append("spc")
        elif ch == "/":
            keys.append("slash")
        elif "a" <= ch <= "z":
            keys.append(ch)
        elif ch.isdigit():
            keys.append(ch)
        else:
            raise SystemExit(f"no qcode for {ch!r}")
    keys.append("ret")
    return tuple(keys)


def wait_for(log_path: str, seconds: float, parse):
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            text = open(log_path, "r", errors="replace").read()
        except FileNotFoundError:
            time.sleep(0.05)
            continue
        found = parse(text)
        if found is not None:
            return found
        time.sleep(0.05)
    raise SystemExit("timed out waiting for the terminal")


def axis(pixel: int, span: int) -> int:
    denom = span - 1
    if denom <= 0:
        raise SystemExit(f"screen span {span} cannot hold the window")
    value = (pixel * AXIS_MAX + denom - 1) // denom
    while value * denom // AXIS_MAX < pixel and value < AXIS_MAX:
        value += 1
    if value * denom // AXIS_MAX != pixel:
        raise SystemExit(f"pixel {pixel} does not land on span {span}")
    return value


def connect(sock_path: str):
    deadline = time.time() + 20
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    while True:
        try:
            sock.connect(sock_path)
            break
        except (FileNotFoundError, ConnectionRefusedError, OSError):
            if time.time() > deadline:
                raise SystemExit("qmp socket did not open")
            time.sleep(0.05)
    return sock.makefile("rwb", buffering=0)


def send_pointer(stream, width: int, height: int) -> None:
    point_x = axis(PIXEL, width)
    point_y = axis(PIXEL, height)
    write_msg(
        stream,
        {
            "execute": "input-send-event",
            "arguments": {
                "events": [
                    {"type": "abs", "data": {"axis": "x", "value": point_x}},
                    {"type": "abs", "data": {"axis": "y", "value": point_y}},
                ],
            },
        },
    )
    reply = read_until_return(stream)
    print(
        f"qmp abs x={point_x} y={point_y} on {width}x{height} reply={reply.strip()}",
        flush=True,
    )


def send_keys(stream, keys, label: str) -> None:
    events = []
    for key in keys:
        events.append(key_event(key, True))
        events.append(key_event(key, False))
    write_msg(
        stream,
        {"execute": "input-send-event", "arguments": {"events": events}},
    )
    reply = read_until_return(stream)
    print(f"qmp keys {label} reply={reply.strip()}", flush=True)


def key_event(key: str, down: bool) -> dict:
    return {
        "type": "key",
        "data": {"down": down, "key": {"type": "qcode", "data": key}},
    }


def read_msg(stream) -> str:
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = stream.read(1)
        if not chunk:
            raise SystemExit("qmp closed")
        buf += chunk
    return buf.decode()


def write_msg(stream, payload) -> None:
    stream.write((json.dumps(payload) + "\n").encode())


def read_until_return(stream) -> str:
    while True:
        message = read_msg(stream)
        if '"return"' in message or '"error"' in message:
            return message


if __name__ == "__main__":
    main()
