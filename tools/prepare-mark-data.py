#!/usr/bin/env python3
"""Convert the PaoPao-Pref archive into two plain domain lists."""
import argparse
import hashlib
import lzma
from pathlib import Path
import re

PACKED_HASH_SIZE = 1024
MAX_UNPACKED_SIZE = 96 * 1024 * 1024
DOMAIN = re.compile(r"domain:[-_.A-Za-z0-9]+\Z")


def convert(source: Path, output: Path) -> None:
    raw = source.read_bytes()
    if len(raw) <= PACKED_HASH_SIZE:
        raise ValueError("the archive does not contain data and a checksum")
    archive = raw[:-PACKED_HASH_SIZE]
    trailer = raw[-PACKED_HASH_SIZE:]
    expected = re.search(rb"[0-9a-fA-F]{128}", trailer)
    if expected is None or hashlib.sha512(archive).hexdigest() != expected[0].lower().decode("ascii"):
        raise ValueError("PaoPao-Pref archive SHA-512 mismatch")

    decoder = lzma.LZMADecompressor(format=lzma.FORMAT_XZ, memlimit=128 * 1024 * 1024)
    unpacked = decoder.decompress(archive, max_length=MAX_UNPACKED_SIZE + 1)
    if not decoder.eof or decoder.unused_data or len(unpacked) > MAX_UNPACKED_SIZE:
        raise ValueError("incomplete or oversized PaoPao-Pref archive")
    known = set()
    china = set()
    for line in unpacked.decode("utf-8-sig").splitlines():
        if DOMAIN.fullmatch(line):
            known.add("domain:" + line.removeprefix("domain:").lstrip("."))
        for marker in ("##@@", "#@"):
            if line.startswith(marker) and DOMAIN.fullmatch(line[len(marker):]):
                china.add("domain:" + line[len(marker):].removeprefix("domain:").lstrip("."))
                break
    if not known or not china:
        raise ValueError("expected both unmarked and CN-marked domain records")

    output.mkdir(parents=True, exist_ok=True)
    for filename, names in (("global_mark.txt", known), ("cn_mark.txt", china)):
        path = output / filename
        tmp = path.with_suffix(".tmp")
        tmp.write_text("\n".join(sorted(names)) + "\n", encoding="utf-8")
        tmp.replace(path)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    try:
        convert(args.source, args.output)
    except (OSError, ValueError, lzma.LZMAError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
