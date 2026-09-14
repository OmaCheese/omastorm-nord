# Cut a NEXRAD Archive II volume (gzip wrapped, uncompressed messages, as the
# 2013 KTLX fixture is) down to its first elevation cut: the 24-byte volume
# header, the metadata messages, and every message up to the first radial
# (message 31) of the second elevation number. That is exactly what
# `engine/src/sweep.rs` (`lowest_reflectivity`) reads before it stops, so the
# decoded sweep cannot change. The kept bytes are a byte-exact prefix of the
# decompressed original; the script asserts that, then gzips it
# deterministically (level 9, no name, mtime 0).
#
#   python3 scripts/trim-level2.py SOURCE.gz EXTRACT.gz
#
# Bzip2-compressed LDM records (live chunks, newer volumes) are refused: this
# only handles the layout the vendored fixture has.
import gzip
import hashlib
import json
import os
import struct
import sys

HEADER = 24  # "AR2V00xx." + volume number, date, time, ICAO
CTM = 12  # the legacy channel terminal manager bytes before each message
FRAME = 2432  # fixed frame of every message but 31


def main():
    source, extract = sys.argv[1], sys.argv[2]
    packed = open(source, "rb").read()
    raw = gzip.decompress(packed) if packed[:2] == b"\x1f\x8b" else packed
    assert raw[:4] == b"AR2V", "not an Archive II volume"
    pos, first, cut, radials, kinds = HEADER, None, None, 0, {}
    while pos + CTM + 16 <= len(raw):
        size_halfwords = struct.unpack(">H", raw[pos + CTM : pos + CTM + 2])[0]
        kind = raw[pos + CTM + 3]
        if kind == 0 and size_halfwords == 0 and pos == HEADER:
            sys.exit("record-size word where a message was expected: bzip2 LDM records are not handled")
        if kind == 31:
            elevation = raw[pos + CTM + 16 + 22]
            if first is None:
                first = elevation
            elif elevation != first:
                cut = pos
                break
            radials += 1
            pos += CTM + 2 * size_halfwords
        else:
            pos += FRAME
        kinds[kind] = kinds.get(kind, 0) + 1
    assert first is not None and cut is not None, "no second elevation cut found"
    kept = raw[:cut]
    assert raw.startswith(kept)
    out = gzip.compress(kept, compresslevel=9, mtime=0)
    assert gzip.decompress(out) == kept
    with open(extract, "wb") as fh:
        fh.write(out)
    print(
        json.dumps(
            {
                "source": os.path.basename(source),
                "sourceBytes": len(packed),
                "sourceSha256": hashlib.sha256(packed).hexdigest(),
                "sourceInflatedBytes": len(raw),
                "extract": os.path.basename(extract),
                "extractBytes": len(out),
                "extractSha256": hashlib.sha256(out).hexdigest(),
                "keptPrefixBytes": cut,
                "keptPrefixSha256": hashlib.sha256(kept).hexdigest(),
                "elevationNumber": first,
                "radials": radials,
                "messagesKept": {str(k): v for k, v in sorted(kinds.items())},
            },
            indent=1,
        )
    )


main()
