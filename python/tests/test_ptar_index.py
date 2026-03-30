"""Tests for ptar_index using synthetic msgpack data."""

from __future__ import annotations

import io
import msgpack
import pytest

from ptar_index import (
    EntryKind,
    PtarIndex,
    TreeEntry,
    describe_raw,
    diff_indexes,
    load_index,
    load_raw,
)


# ---------------------------------------------------------------------------
# Helpers — build synthetic index blobs
# ---------------------------------------------------------------------------

def _make_file_node(name: str, path: str, size: int, sha256: bytes | None = None) -> dict:
    node: dict = {
        "name": name,
        "path": path,
        "kind": "File",
        "size": size,
    }
    if sha256 is not None:
        node["hash"] = sha256
    return node


def _make_dir_node(
    name: str, path: str, children: list, *, size: int = 0, file_count: int = 0, dir_count: int = 0
) -> dict:
    return {
        "name": name,
        "path": path,
        "kind": "Directory",
        "size": size,
        "file_count": file_count,
        "dir_count": dir_count,
        "children": children,
        "hash": b"\xaa" * 32,
    }


def _pack_to_tempfile(obj, tmp_path, name="test.idx"):
    p = tmp_path / name
    p.write_bytes(msgpack.packb(obj, use_bin_type=True))
    return p


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

FAKE_HASH_A = b"\x01" * 32
FAKE_HASH_B = b"\x02" * 32


@pytest.fixture
def simple_tree():
    """A small tree:  /data  ->  a.txt, b.txt, sub/  ->  c.txt"""
    return {
        "root_path": "/data",
        "tree": _make_dir_node(
            "data",
            "/data",
            [
                _make_file_node("a.txt", "/data/a.txt", 100, FAKE_HASH_A),
                _make_file_node("b.txt", "/data/b.txt", 200, FAKE_HASH_B),
                _make_dir_node(
                    "sub",
                    "/data/sub",
                    [_make_file_node("c.txt", "/data/sub/c.txt", 50)],
                    size=50,
                    file_count=1,
                ),
            ],
            size=350,
            file_count=3,
            dir_count=1,
        ),
    }


@pytest.fixture
def simple_idx(tmp_path, simple_tree):
    return _pack_to_tempfile(simple_tree, tmp_path, "example.idx")


@pytest.fixture
def simple_etr(tmp_path):
    """Empty tree — no hashes or sizes."""
    tree = {
        "root_path": "/data",
        "tree": _make_dir_node(
            "data",
            "/data",
            [
                {"name": "a.txt", "path": "/data/a.txt", "kind": "File"},
                {"name": "sub", "path": "/data/sub", "kind": "Directory", "children": []},
            ],
        ),
    }
    return _pack_to_tempfile(tree, tmp_path, "example.etr")


# ---------------------------------------------------------------------------
# Tests: loading
# ---------------------------------------------------------------------------

class TestLoadRaw:
    def test_roundtrip(self, simple_idx):
        raw = load_raw(simple_idx)
        assert isinstance(raw, dict)
        assert "root_path" in raw
        assert "tree" in raw

    def test_from_file_object(self, simple_idx):
        with open(simple_idx, "rb") as f:
            raw = load_raw(f)
        assert isinstance(raw, dict)


class TestLoadIndex:
    def test_basic_load(self, simple_idx):
        idx = load_index(simple_idx)
        assert isinstance(idx, PtarIndex)
        assert idx.root_path == "/data"
        assert idx.file_type == "idx"

    def test_etr_type(self, simple_etr):
        idx = load_index(simple_etr)
        assert idx.file_type == "etr"

    def test_root_entry(self, simple_idx):
        idx = load_index(simple_idx)
        assert idx.root.name == "data"
        assert idx.root.path == "/data"
        assert idx.root.is_dir

    def test_file_count(self, simple_idx):
        idx = load_index(simple_idx)
        assert idx.total_files == 3

    def test_total_size(self, simple_idx):
        idx = load_index(simple_idx)
        assert idx.total_size == 350

    def test_debug_mode(self, simple_idx, capsys):
        load_index(simple_idx, debug=True)
        captured = capsys.readouterr()
        assert "Raw type" in captured.err


# ---------------------------------------------------------------------------
# Tests: tree navigation
# ---------------------------------------------------------------------------

class TestTreeEntry:
    def test_getitem(self, simple_idx):
        idx = load_index(simple_idx)
        a = idx.root["a.txt"]
        assert a.name == "a.txt"
        assert a.is_file

    def test_getitem_missing(self, simple_idx):
        idx = load_index(simple_idx)
        with pytest.raises(KeyError):
            idx.root["nope.txt"]

    def test_contains(self, simple_idx):
        idx = load_index(simple_idx)
        assert "a.txt" in idx.root
        assert "nope" not in idx.root

    def test_len(self, simple_idx):
        idx = load_index(simple_idx)
        assert len(idx.root) == 3  # a.txt, b.txt, sub/

    def test_iter(self, simple_idx):
        idx = load_index(simple_idx)
        names = [c.name for c in idx.root]
        assert "a.txt" in names
        assert "sub" in names

    def test_resolve(self, simple_idx):
        idx = load_index(simple_idx)
        c = idx.resolve("sub/c.txt")
        assert c is not None
        assert c.name == "c.txt"
        assert c.size == 50

    def test_resolve_missing(self, simple_idx):
        idx = load_index(simple_idx)
        assert idx.resolve("sub/nope") is None

    def test_parent_pointers(self, simple_idx):
        idx = load_index(simple_idx)
        c = idx.resolve("sub/c.txt")
        assert c.parent is not None
        assert c.parent.name == "sub"
        assert c.parent.parent is idx.root

    def test_depth(self, simple_idx):
        idx = load_index(simple_idx)
        assert idx.root.depth == 0
        assert idx.root["a.txt"].depth == 1
        assert idx.resolve("sub/c.txt").depth == 2


# ---------------------------------------------------------------------------
# Tests: traversal
# ---------------------------------------------------------------------------

class TestTraversal:
    def test_walk(self, simple_idx):
        idx = load_index(simple_idx)
        all_entries = list(idx.walk())
        # root + a.txt + b.txt + sub + c.txt = 5
        assert len(all_entries) == 5

    def test_walk_files(self, simple_idx):
        idx = load_index(simple_idx)
        files = list(idx.walk_files())
        assert len(files) == 3
        assert all(f.is_file for f in files)

    def test_walk_dirs(self, simple_idx):
        idx = load_index(simple_idx)
        dirs = list(idx.walk_dirs())
        names = [d.name for d in dirs]
        assert "data" in names
        assert "sub" in names

    def test_find(self, simple_idx):
        idx = load_index(simple_idx)
        big = list(idx.find(lambda e: e.size is not None and e.size >= 100))
        paths = [e.path for e in big]
        assert "/data/a.txt" in paths
        assert "/data/b.txt" in paths

    def test_glob(self, simple_idx):
        idx = load_index(simple_idx)
        txts = list(idx.glob("**/*.txt"))
        assert len(txts) == 3


# ---------------------------------------------------------------------------
# Tests: hashes
# ---------------------------------------------------------------------------

class TestHashes:
    def test_hash_hex(self, simple_idx):
        idx = load_index(simple_idx)
        a = idx.root["a.txt"]
        assert a.hash_hex == FAKE_HASH_A.hex()
        assert a.hash_hex == "01" * 32

    def test_root_hash(self, simple_idx):
        idx = load_index(simple_idx)
        assert idx.root_hash is not None


# ---------------------------------------------------------------------------
# Tests: diff
# ---------------------------------------------------------------------------

class TestDiff:
    def test_no_diff(self, tmp_path, simple_tree):
        p1 = _pack_to_tempfile(simple_tree, tmp_path, "a.idx")
        p2 = _pack_to_tempfile(simple_tree, tmp_path, "b.idx")
        d = load_index(p1).diff(load_index(p2))
        assert not d.has_differences
        assert d.summary() == "no differences"

    def test_added_file(self, tmp_path, simple_tree):
        import copy

        tree2 = copy.deepcopy(simple_tree)
        tree2["tree"]["children"].append(
            _make_file_node("d.txt", "/data/d.txt", 999)
        )
        p1 = _pack_to_tempfile(simple_tree, tmp_path, "a.idx")
        p2 = _pack_to_tempfile(tree2, tmp_path, "b.idx")
        d = load_index(p1).diff(load_index(p2))
        assert len(d.added) == 1
        assert d.added[0].name == "d.txt"

    def test_removed_file(self, tmp_path, simple_tree):
        import copy

        tree2 = copy.deepcopy(simple_tree)
        tree2["tree"]["children"] = [
            c for c in tree2["tree"]["children"] if c.get("name") != "a.txt"
        ]
        p1 = _pack_to_tempfile(simple_tree, tmp_path, "a.idx")
        p2 = _pack_to_tempfile(tree2, tmp_path, "b.idx")
        d = load_index(p1).diff(load_index(p2))
        assert len(d.removed) == 1
        assert d.removed[0].name == "a.txt"

    def test_changed_file(self, tmp_path, simple_tree):
        import copy

        tree2 = copy.deepcopy(simple_tree)
        for c in tree2["tree"]["children"]:
            if c.get("name") == "a.txt":
                c["size"] = 9999
                c["hash"] = b"\xff" * 32
        p1 = _pack_to_tempfile(simple_tree, tmp_path, "a.idx")
        p2 = _pack_to_tempfile(tree2, tmp_path, "b.idx")
        d = load_index(p1).diff(load_index(p2))
        assert len(d.changed) == 1
        assert "size" in d.changed[0].changes[0]


# ---------------------------------------------------------------------------
# Tests: export
# ---------------------------------------------------------------------------

class TestExport:
    def test_to_dict(self, simple_idx):
        idx = load_index(simple_idx)
        d = idx.to_dict()
        assert d["root_path"] == "/data"
        assert "tree" in d
        assert d["tree"]["kind"] == "DIRECTORY"

    def test_to_json(self, simple_idx):
        idx = load_index(simple_idx)
        j = idx.to_json()
        assert '"root_path"' in j

    def test_to_flat_list(self, simple_idx):
        idx = load_index(simple_idx)
        rows = idx.to_flat_list()
        assert len(rows) == 3
        assert all("path" in r for r in rows)

    def test_print_tree(self, simple_idx, capsys):
        idx = load_index(simple_idx)
        idx.print_tree(max_depth=1)
        out = capsys.readouterr().out
        assert "data" in out
        assert "a.txt" in out


# ---------------------------------------------------------------------------
# Tests: describe_raw
# ---------------------------------------------------------------------------

class TestDescribeRaw:
    def test_basic(self, simple_idx):
        desc = describe_raw(simple_idx)
        assert "dict" in desc
        assert "root_path" in desc


# ---------------------------------------------------------------------------
# Tests: human_size
# ---------------------------------------------------------------------------

class TestHumanSize:
    def test_file_size(self, simple_idx):
        idx = load_index(simple_idx)
        a = idx.root["a.txt"]
        assert a.human_size == "100 B"

    def test_none_size(self):
        e = TreeEntry()
        assert e.human_size == "—"
