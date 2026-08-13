#!/usr/bin/env python3
"""Allocate distinct test ports outside Linux's ephemeral source-port range."""

from __future__ import annotations

import socket
import sys
from collections.abc import Callable
from itertools import chain


EPHEMERAL_RANGE_PATH = "/proc/sys/net/ipv4/ip_local_port_range"


def read_ephemeral_range(path: str = EPHEMERAL_RANGE_PATH) -> tuple[int, int]:
    with open(path, encoding="ascii") as handle:
        fields = handle.read().split()
    if len(fields) != 2:
        raise RuntimeError(f"invalid ephemeral port range in {path}")
    try:
        low, high = map(int, fields)
    except ValueError as error:
        raise RuntimeError(f"invalid ephemeral port range in {path}") from error
    if not 1024 <= low <= high <= 65535:
        raise RuntimeError(f"invalid ephemeral port range in {path}: {low} {high}")
    return low, high


def allocate_ports(
    count: int,
    *,
    ephemeral_range: tuple[int, int] | None = None,
    socket_factory: Callable[..., socket.socket] = socket.socket,
) -> list[int]:
    if count < 1:
        raise ValueError("port count must be positive")
    low, high = ephemeral_range or read_ephemeral_range()
    if not 1024 <= low <= high <= 65535:
        raise ValueError(f"invalid ephemeral port range: {low} {high}")
    candidates = chain(range(1024, low), range(high + 1, 65536))
    reservations: list[socket.socket] = []
    ports: list[int] = []
    try:
        for port in candidates:
            reservation = socket_factory(socket.AF_INET, socket.SOCK_STREAM)
            try:
                # A wildcard reservation proves the port is available to both
                # the loopback-only Gateway and the host-published fixtures.
                reservation.bind(("0.0.0.0", port))
                reservation.listen(1)
            except OSError:
                reservation.close()
                continue
            reservations.append(reservation)
            ports.append(port)
            if len(ports) == count:
                return ports
    finally:
        for reservation in reservations:
            reservation.close()
    raise RuntimeError(f"could not reserve {count} ports outside {low}-{high}")


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        raise SystemExit(f"usage: {argv[0]} COUNT")
    try:
        count = int(argv[1])
    except ValueError as error:
        raise SystemExit("COUNT must be an integer") from error
    for port in allocate_ports(count):
        print(port)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
