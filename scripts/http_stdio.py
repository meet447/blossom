#!/usr/bin/env python3
"""Answer one HTTP GET on stdin with a fixed 200 body."""

import sys

BODY = b"meuxe-alpha"
RESP = (
    b"HTTP/1.0 200 OK\r\n"
    b"Content-Length: 11\r\n"
    b"\r\n"
    + BODY
)


def main() -> None:
    sys.stdin.buffer.read()
    sys.stdout.buffer.write(RESP)
    sys.stdout.buffer.flush()


if __name__ == "__main__":
    main()
