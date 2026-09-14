# Cut an OPERA composite (ODIM_H5 COMP, one float64 DBZH layer of 4400 x
# 3800 in chunks of 760 x 880) down to a test fixture: the root attributes
# and /what, /where, /how unchanged, /dataset1/what, and /dataset1/data1 with
# its what and its data, of which only the chunks named by --keep are
# allocated (the DBZH quality layer is dropped). Kept chunks are copied as
# stored (H5Dread_chunk / H5Dwrite_chunk), so their bytes are the source's
# own; every other chunk is left unallocated and reads as the dataset's fill
# value, which is set to the layer's nodata. The file is written with the
# source's file-creation properties, capped at the 1.8 format, so its
# superblock has the source's shape (version 0, 8-byte offsets).
#
#   uv run --no-project --with h5py python scripts/crop-opera.py \
#     SOURCE.h5 FIXTURE.h5 --keep 1,2 2,2 [--max-bytes 300000]
#
# --keep takes chunk (row, column) pairs, counted in chunks from the
# north-west corner. The Nordic box needs 11 of the 30 (golden/nordic-*/
# grid.json, neededChunks), about 0.87 MB, so a fixture under 300 KB keeps
# only some of them (data/README.md). After writing, the script re-opens
# both files and checks the attributes, the layout, and every kept chunk
# byte for byte; it fails when the fixture is larger than --max-bytes.
import argparse
import hashlib
import json
import os
import sys

import h5py
import numpy as np

LAYER = "dataset1/data1"


def text(v):
    if isinstance(v, bytes):
        v = v.decode("ascii")
    return str(v).rstrip("\0").strip()


def copy_attrs(src, dst):
    for k, v in src.attrs.items():
        dst.attrs.create(k, v, dtype=src.attrs.get_id(k).dtype)


def same_attrs(a, b, where):
    assert sorted(a.keys()) == sorted(b.keys()), f"{where}: attribute names differ"
    for k in a.keys():
        assert a.get_id(k).dtype == b.get_id(k).dtype, f"{where}@{k}: type differs"
        assert np.array_equal(np.asarray(a[k]), np.asarray(b[k])), f"{where}@{k}: value differs"


def main():
    p = argparse.ArgumentParser()
    p.add_argument("source")
    p.add_argument("fixture")
    p.add_argument("--keep", nargs="+", required=True, help="chunks to keep, as ROW,COL")
    p.add_argument("--max-bytes", type=int, default=300_000)
    args = p.parse_args()
    keep = [tuple(int(n) for n in pair.split(",")) for pair in args.keep]

    src = h5py.File(args.source, "r")
    assert text(src["what"].attrs["object"]) == "COMP", "not an ODIM composite"
    assert text(src[f"{LAYER}/what"].attrs["quantity"]) == "DBZH", f"{LAYER} is not DBZH"
    layer = src[f"{LAYER}/data"]
    rows, cols = layer.chunks
    nodata = float(src[f"{LAYER}/what"].attrs["nodata"])
    stored = {}
    for i in range(layer.id.get_num_chunks()):
        info = layer.id.get_chunk_info(i)
        stored[(info.chunk_offset[0] // rows, info.chunk_offset[1] // cols)] = info.chunk_offset
    missing = [k for k in keep if k not in stored]
    assert not missing, f"the source allocates no chunk {missing}"

    if os.path.exists(args.fixture):
        os.remove(args.fixture)
    fcpl = src.id.get_create_plist()
    fapl = h5py.h5p.create(h5py.h5p.FILE_ACCESS)
    fapl.set_libver_bounds(h5py.h5f.LIBVER_EARLIEST, h5py.h5f.LIBVER_V18)
    out = h5py.File(h5py.h5f.create(os.fsencode(args.fixture), h5py.h5f.ACC_EXCL, fcpl=fcpl, fapl=fapl))
    copy_attrs(src, out)
    for k in ("what", "where", "how"):
        src.copy(src[k], out, name=k)
    group = out.create_group("dataset1")
    copy_attrs(src["dataset1"], group)
    src.copy(src["dataset1/what"], group, name="what")
    data1 = group.create_group("data1")
    copy_attrs(src[LAYER], data1)
    src.copy(src[f"{LAYER}/what"], data1, name="what")
    dcpl = layer.id.get_create_plist()
    dcpl.set_fill_value(np.array(nodata, dtype=layer.dtype))
    dcpl.set_fill_time(h5py.h5d.FILL_TIME_IFSET)
    space = h5py.h5s.create_simple(layer.shape)
    dset = h5py.h5d.create(data1.id, b"data", h5py.h5t.py_create(layer.dtype), space, dcpl=dcpl)
    written = h5py.Dataset(dset)
    copy_attrs(layer, written)
    for k in keep:
        mask, raw = layer.id.read_direct_chunk(stored[k])
        dset.write_direct_chunk(stored[k], raw, mask)
    out.close()

    # Check what was written, from the file on disk.
    dst = h5py.File(args.fixture, "r")
    same_attrs(src.attrs, dst.attrs, "/")
    for k in ("what", "where", "how", "dataset1/what", f"{LAYER}/what"):
        same_attrs(src[k].attrs, dst[k].attrs, f"/{k}")
    same_attrs(layer.attrs, dst[f"{LAYER}/data"].attrs, f"/{LAYER}/data")
    got = dst[f"{LAYER}/data"]
    assert (got.dtype, got.shape, got.chunks, got.compression) == (
        layer.dtype, layer.shape, layer.chunks, layer.compression), "layout differs"
    assert got.id.get_num_chunks() == len(keep), "chunk count differs"
    for k in keep:
        assert got.id.read_direct_chunk(stored[k]) == layer.id.read_direct_chunk(stored[k]), f"chunk {k} differs"
    assert float(got.fillvalue) == nodata, "fill value is not nodata"

    raw = open(args.source, "rb").read()
    data = open(args.fixture, "rb").read()
    report = {
        "source": os.path.basename(args.source),
        "sourceBytes": len(raw),
        "sourceSha256": hashlib.sha256(raw).hexdigest(),
        "fixture": os.path.basename(args.fixture),
        "fixtureBytes": len(data),
        "fixtureSha256": hashlib.sha256(data).hexdigest(),
        "kept": [{"row": r, "col": c, "bytes": int(got.id.get_chunk_info_by_coord(stored[(r, c)]).size)}
                 for r, c in keep],
        "h5py": h5py.__version__,
        "hdf5": h5py.version.hdf5_version,
    }
    print(json.dumps(report, indent=1))
    if len(data) > args.max_bytes:
        sys.exit(f"fixture is {len(data)} bytes, over --max-bytes {args.max_bytes}")


main()
