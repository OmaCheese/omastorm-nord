# Cut an ODIM_H5 polar volume (PVOL, or a single-sweep SCAN) down to a test fixture: the root groups
# (/what, /where, /how and the root attributes, unchanged) plus the
# lowest-elevation dataset, written as /dataset1 with only the quantities
# asked for (DBZH by default). Everything kept is copied with H5Ocopy, so
# attribute types and the stored (compressed) chunks are the source's own
# bytes, and the new file is written with the source's file-creation
# properties (superblock version, 4-byte offsets and lengths, B-tree ranks),
# so the extract has the same shape the range reader sees from SMHI. The
# root groups still describe the whole volume (they are not rewritten).
#
#   uv run --no-project --with h5py python scripts/trim-odim.py \
#     SOURCE.h5 EXTRACT.h5 [--keep DBZH,TH] [--tilts lowest|all|0,2,5] [--max-bytes 300000]
#
# --tilts (S20, the multi-angle fixtures) keeps more than the lowest tilt:
# `all`, or positions in ascending elevation (0 = the lowest). The kept
# tilts become /dataset1.. in ascending elevation, each with only the
# quantities asked for.
#
# After writing, the script re-opens both files and checks: the superblock's
# end-of-file address equals the file length (the S3 range reader's
# assumption), every kept attribute equals the source's, and every kept
# chunk is byte-identical to the source's chunk. It fails when the extract
# is larger than --max-bytes (data/README.md: at most 300 KB per country).
import argparse
import hashlib
import json
import os
import struct
import sys

import h5py
import numpy as np


def text(v):
    if isinstance(v, bytes):
        v = v.decode("ascii")
    return str(v).rstrip("\0").strip()


def superblock_eof(head):
    """The end-of-file address an HDF5 superblock (version 0-3) records."""
    assert head[:8] == b"\x89HDF\r\n\x1a\n", "no HDF5 signature at byte 0"
    version = head[8]
    if version in (0, 1):
        size_o = head[13]
        base = 24 + (4 if version == 1 else 0)
        index = 2  # base, free-space info, end of file
    else:
        size_o = head[9]
        base = 12
        index = 2  # base, superblock extension, end of file
    fmt = {4: "<I", 8: "<Q"}[size_o]
    at = base + index * size_o
    return version, size_o, struct.unpack(fmt, head[at : at + size_o])[0]


def same_attrs(a, b, where):
    assert sorted(a.keys()) == sorted(b.keys()), f"{where}: attribute names differ"
    for k in a.keys():
        x, y = a[k], b[k]
        assert a.get_id(k).dtype == b.get_id(k).dtype, f"{where}@{k}: type differs"
        assert np.array_equal(np.asarray(x), np.asarray(y)), f"{where}@{k}: value differs"


def same_tree(src, dst, where):
    """Attributes, subgroups and datasets of dst equal src's, chunk for chunk."""
    same_attrs(src.attrs, dst.attrs, where)
    assert sorted(src.keys()) == sorted(dst.keys()), f"{where}: members differ"
    for k in src.keys():
        s, d = src[k], dst[k]
        if isinstance(s, h5py.Group):
            same_tree(s, d, f"{where}/{k}")
            continue
        assert s.dtype == d.dtype and s.shape == d.shape, f"{where}/{k}: shape differs"
        assert s.compression == d.compression and s.chunks == d.chunks, f"{where}/{k}: layout differs"
        same_attrs(s.attrs, d.attrs, f"{where}/{k}")
        if s.chunks:
            n = s.id.get_num_chunks()
            assert n == d.id.get_num_chunks(), f"{where}/{k}: chunk count differs"
            for i in range(n):
                offset = s.id.get_chunk_info(i).chunk_offset
                assert s.id.read_direct_chunk(offset) == d.id.read_direct_chunk(offset), (
                    f"{where}/{k}: chunk {offset} differs"
                )
        assert np.array_equal(s[...], d[...]), f"{where}/{k}: values differ"


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("source")
    p.add_argument("extract")
    p.add_argument("--keep", default="DBZH", help="comma-separated quantities of each kept tilt to keep")
    p.add_argument(
        "--tilts", default="lowest", help="lowest, all, or positions in ascending elevation (0 = lowest)"
    )
    p.add_argument("--max-bytes", type=int, default=300_000)
    args = p.parse_args()
    keep = [q.strip() for q in args.keep.split(",") if q.strip()]

    src = h5py.File(args.source, "r")
    assert text(src["what"].attrs["object"]) in ("PVOL", "SCAN"), "not an ODIM polar volume or scan"
    tilts = {k: float(src[k]["where"].attrs["elangle"]) for k in src if k.startswith("dataset")}
    ascending = sorted(tilts, key=lambda k: (tilts[k], int(k[len("dataset") :])))
    lowest = ascending[0]
    if args.tilts == "lowest":
        chosen = [lowest]
    elif args.tilts == "all":
        chosen = ascending
    else:
        chosen = [ascending[int(i)] for i in args.tilts.split(",")]
        assert chosen == sorted(chosen, key=ascending.index), "--tilts must ascend"
    # Per kept tilt: its new name, source name, kept moments (source order,
    # renumbered from data1) and its other members (what, where, how, quality*).
    plan = []
    for n, name in enumerate(chosen, 1):
        ds = src[name]
        moments = sorted((k for k in ds if k.startswith("data")), key=lambda k: int(k[len("data") :]))
        quantity = {k: text(ds[k]["what"].attrs["quantity"]) for k in moments}
        missing = [q for q in keep if q not in quantity.values()]
        assert not missing, f"{name} has no {missing}; it has {sorted(quantity.values())}"
        kept = [k for k in moments if quantity[k] in keep]
        others = [k for k in ds if not k.startswith("data")]
        renamed = {f"data{m}": k for m, k in enumerate(kept, 1)}
        plan.append((f"dataset{n}", name, renamed, others, quantity))

    if os.path.exists(args.extract):
        os.remove(args.extract)
    fcpl = src.id.get_create_plist()
    # HDF5 2.0 writes superblock version 2 for these creation properties
    # unless the format is capped at 1.8, which gives SMHI's version 1.
    fapl = h5py.h5p.create(h5py.h5p.FILE_ACCESS)
    fapl.set_libver_bounds(h5py.h5f.LIBVER_EARLIEST, h5py.h5f.LIBVER_V18)
    out = h5py.File(h5py.h5f.create(os.fsencode(args.extract), h5py.h5f.ACC_EXCL, fcpl=fcpl, fapl=fapl))
    for k, v in src.attrs.items():
        out.attrs.create(k, v, dtype=src.attrs.get_id(k).dtype)
    for k in ("what", "where", "how"):
        if k in src:
            src.copy(src[k], out, name=k)
    for new_name, name, renamed, others, _ in plan:
        ds = src[name]
        group = out.create_group(new_name)
        for k, v in ds.attrs.items():
            group.attrs.create(k, v, dtype=ds.attrs.get_id(k).dtype)
        for k in others:
            src.copy(ds[k], group, name=k)
        for new, old in renamed.items():
            src.copy(ds[old], group, name=new)
    out.close()

    # Check what was written, from the bytes on disk.
    data = open(args.extract, "rb").read()
    version, size_o, eof = superblock_eof(data)
    assert eof == len(data), f"superblock end-of-file {eof} != file length {len(data)}"
    assert (version, size_o) == superblock_eof(open(args.source, "rb").read(64))[:2], "superblock shape differs"
    dst = h5py.File(args.extract, "r")
    assert sorted(dst.keys()) == sorted(
        [new_name for new_name, *_ in plan] + [k for k in ("what", "where", "how") if k in src]
    )
    same_attrs(src.attrs, dst.attrs, "/")
    for k in ("what", "where", "how"):
        if k in src:
            same_tree(src[k], dst[k], f"/{k}")
    for new_name, name, renamed, others, _ in plan:
        ds, group = src[name], dst[new_name]
        same_attrs(ds.attrs, group.attrs, f"/{new_name}")
        assert sorted(group.keys()) == sorted(others + list(renamed)), f"{new_name} members differ"
        for k in others:
            same_tree(ds[k], group[k], f"/{new_name}/{k}")
        for new, old in renamed.items():
            same_tree(ds[old], group[new], f"/{new_name}/{new}")

    raw = open(args.source, "rb").read()
    report = {
        "source": os.path.basename(args.source),
        "sourceBytes": len(raw),
        "sourceSha256": hashlib.sha256(raw).hexdigest(),
        "extract": os.path.basename(args.extract),
        "extractBytes": len(data),
        "extractSha256": hashlib.sha256(data).hexdigest(),
        "lowestTilt": f"/{lowest} (elangle {tilts[lowest]})",
        "tilts": {f"/{new_name}": f"/{name} (elangle {tilts[name]})" for new_name, name, *_ in plan},
        "kept": {
            f"/{new_name}/{n}": f"/{name}/{o} {quantity[o]}"
            for new_name, name, renamed, _, quantity in plan
            for n, o in renamed.items()
        },
        "superblock": {"version": version, "offsetBytes": size_o, "eofAddress": eof},
        "h5py": h5py.__version__,
        "hdf5": h5py.version.hdf5_version,
    }
    print(json.dumps(report, indent=1))
    if len(data) > args.max_bytes:
        sys.exit(f"extract is {len(data)} bytes, over --max-bytes {args.max_bytes}")


main()
