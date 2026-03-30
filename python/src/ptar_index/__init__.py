"""
ptar_index — Python interface for parallel-tar (.etr / .idx) index files.

parallel-tar (https://github.com/JBlaschke/parallel-tar) serializes its
tree indices with rmp-serde (Rust MessagePack + Serde).  This module
deserializes those files into native Python dataclasses and provides
utilities for traversal, querying, diffing, and export.

Requirements
------------
    pip install msgpack

Usage
-----
    from ptar_index import load_index

    idx = load_index("example.idx")   # or .etr
    print(idx)                         # pretty summary
    idx.root.print_tree(max_depth=2)   # show first 2 levels

    # iterate every file entry
    for entry in idx.walk_files():
        print(entry.path, entry.size, entry.hash_hex)

    # search by glob
    for entry in idx.glob("**/*.tar"):
        print(entry.path)
"""

from __future__ import annotations

import fnmatch
import os
import struct
from dataclasses import dataclass, field
from enum import Enum, auto
from pathlib import PurePosixPath
from typing import (
    Any,
    BinaryIO,
    Callable,
    Dict,
    Generator,
    List,
    Optional,
    Tuple,
    Union,
)

try:
    import msgpack
except ImportError:
    raise ImportError(
        "The 'msgpack' package is required.  Install it with:\n"
        "    pip install msgpack\n"
        "or:\n"
        "    uv add msgpack"
    )

__all__ = [
    "load_index",
    "load_raw",
    "describe_raw",
    "PtarIndex",
    "TreeEntry",
    "EntryKind",
    "IndexDiff",
    "ChangedEntry",
    "diff_indexes",
]


# ---------------------------------------------------------------------------
# Low-level: raw MessagePack loading
# ---------------------------------------------------------------------------

def load_raw(path_or_fp: Union[str, os.PathLike, BinaryIO]) -> Any:
    """
    Load a .etr or .idx file and return the raw Python object (dicts, lists,
    bytes, ints, strings …) that msgpack produces.

    This is the escape-hatch for inspecting the binary format before the
    higher-level wrappers interpret it.
    """
    if isinstance(path_or_fp, (str, os.PathLike)):
        with open(path_or_fp, "rb") as fp:
            return _unpack(fp)
    return _unpack(path_or_fp)


def _unpack(fp: BinaryIO) -> Any:
    unpacker = msgpack.Unpacker(fp, raw=False, strict_map_key=False)
    result = None
    for obj in unpacker:
        if result is not None:
            if not isinstance(result, list):
                result = [result]
            result.append(obj)
        else:
            result = obj
    return result


# ---------------------------------------------------------------------------
# Data model
# ---------------------------------------------------------------------------

class EntryKind(Enum):
    """Whether a tree node represents a file or a directory."""
    FILE = auto()
    DIRECTORY = auto()
    SYMLINK = auto()
    UNKNOWN = auto()


@dataclass
class TreeEntry:
    """
    A single node in the parallel-tar index tree.

    Directories have *children*; files / symlinks are leaves. Metadata fields
    (size, hash, …) may be ``None`` for empty-tree (.etr) files.
    """

    # --- identity ----------------------------------------------------------
    name: str = ""
    """Basename of this entry (e.g. ``"data"``)."""

    path: str = ""
    """Full path as stored in the index."""

    kind: EntryKind = EntryKind.UNKNOWN

    # --- metadata (may be None for .etr) -----------------------------------
    size: Optional[int] = None
    """File size in bytes (individual file) or cumulative (directory)."""

    hash_bytes: Optional[bytes] = None
    """Raw hash bytes (SHA-256 or MD5, depending on tool config)."""

    hash_str: Optional[str] = None
    """Original hash string as stored in the index (hex-encoded)."""

    md5_bytes: Optional[bytes] = None
    """MD5 hash bytes, if present separately."""

    mode: Optional[int] = None
    """Unix file mode / permissions."""

    mtime: Optional[float] = None
    """Modification time as a Unix timestamp."""

    uid: Optional[int] = None
    gid: Optional[int] = None

    # --- tree structure ----------------------------------------------------
    children: List["TreeEntry"] = field(default_factory=list)
    """Child entries (only populated for directories)."""

    parent: Optional["TreeEntry"] = field(default=None, repr=False)
    """Back-pointer to the parent node (not serialized)."""

    # --- aggregate stats (directories) -------------------------------------
    file_count: Optional[int] = None
    """Number of files (recursive) under this directory."""

    dir_count: Optional[int] = None
    """Number of sub-directories (recursive) under this directory."""

    # --- raw data for anything we didn't map -------------------------------
    _raw: Any = field(default=None, repr=False)

    # --- derived properties ------------------------------------------------
    @property
    def hash_hex(self) -> Optional[str]:
        """Hex-encoded hash string, or ``None``."""
        if self.hash_str:
            return self.hash_str
        if self.hash_bytes:
            return self.hash_bytes.hex()
        return None

    @property
    def md5_hex(self) -> Optional[str]:
        if self.md5_bytes:
            return self.md5_bytes.hex()
        return None

    @property
    def is_dir(self) -> bool:
        return self.kind == EntryKind.DIRECTORY

    @property
    def is_file(self) -> bool:
        return self.kind == EntryKind.FILE

    @property
    def depth(self) -> int:
        """Depth from the root (root = 0)."""
        d = 0
        node = self.parent
        while node is not None:
            d += 1
            node = node.parent
        return d

    @property
    def human_size(self) -> str:
        """Human-readable size string."""
        if self.size is None:
            return "—"
        return _human_bytes(self.size)

    # --- child access ------------------------------------------------------
    def child(self, name: str) -> Optional["TreeEntry"]:
        """Look up an immediate child by basename."""
        for c in self.children:
            if c.name == name:
                return c
        return None

    def __getitem__(self, key: str) -> "TreeEntry":
        """Look up a child by name; raises KeyError if missing."""
        c = self.child(key)
        if c is None:
            raise KeyError(key)
        return c

    def __contains__(self, key: str) -> bool:
        return self.child(key) is not None

    def __len__(self) -> int:
        return len(self.children)

    def __iter__(self):
        return iter(self.children)

    # --- traversal ---------------------------------------------------------
    def walk(self) -> Generator["TreeEntry", None, None]:
        """Yield this node and all descendants depth-first."""
        yield self
        for c in self.children:
            yield from c.walk()

    def walk_files(self) -> Generator["TreeEntry", None, None]:
        """Yield only file entries (leaves)."""
        for e in self.walk():
            if e.is_file:
                yield e

    def walk_dirs(self) -> Generator["TreeEntry", None, None]:
        """Yield only directory entries."""
        for e in self.walk():
            if e.is_dir:
                yield e

    def find(
        self, predicate: Callable[["TreeEntry"], bool]
    ) -> Generator["TreeEntry", None, None]:
        """Yield entries matching an arbitrary predicate."""
        for e in self.walk():
            if predicate(e):
                yield e

    def glob(self, pattern: str) -> Generator["TreeEntry", None, None]:
        """
        Yield entries whose *path* matches a glob pattern.

        Supports ``*``, ``?``, ``[seq]`` and ``**`` (recursive).
        """
        for e in self.walk():
            if _glob_match(e.path, pattern):
                yield e

    def resolve(self, relpath: str) -> Optional["TreeEntry"]:
        """
        Resolve a '/'-separated relative path from this node.

        >>> root.resolve("LCLS/sit_psdm_data/psdm")
        """
        parts = [p for p in relpath.strip("/").split("/") if p]
        node = self
        for p in parts:
            node = node.child(p)
            if node is None:
                return None
        return node

    # --- display -----------------------------------------------------------
    def print_tree(
        self,
        *,
        max_depth: Optional[int] = None,
        show_size: bool = True,
        show_hash: bool = False,
        _prefix: str = "",
        _is_last: bool = True,
        _depth: int = 0,
        _file=None,
    ) -> None:
        """Pretty-print the tree to stdout (or *_file*)."""
        import sys

        out = _file or sys.stdout

        connector = "└── " if _is_last else "├── "
        if _depth == 0:
            label = self.path or self.name or "/"
        else:
            label = self.name

        extras: list[str] = []
        if show_size and self.size is not None:
            extras.append(self.human_size)
        if show_hash and self.hash_hex:
            extras.append(self.hash_hex[:16])
        if self.is_dir and self.file_count is not None:
            extras.append(f"{self.file_count} files")
        suffix = f"  ({', '.join(extras)})" if extras else ""

        if _depth == 0:
            print(f"{label}{suffix}", file=out)
        else:
            print(f"{_prefix}{connector}{label}{suffix}", file=out)

        if max_depth is not None and _depth >= max_depth:
            if self.children:
                child_prefix = _prefix + ("    " if _is_last else "│   ")
                print(f"{child_prefix}└── …", file=out)
            return

        child_prefix = _prefix + ("    " if _is_last else "│   ")
        for i, child in enumerate(self.children):
            child.print_tree(
                max_depth=max_depth,
                show_size=show_size,
                show_hash=show_hash,
                _prefix=child_prefix,
                _is_last=(i == len(self.children) - 1),
                _depth=_depth + 1,
                _file=out,
            )

    def to_dict(self, *, include_children: bool = True) -> dict:
        """Recursively export to a plain dict / JSON-friendly structure."""
        d: dict = {
            "name": self.name,
            "path": self.path,
            "kind": self.kind.name,
        }
        if self.size is not None:
            d["size"] = self.size
        if self.hash_hex:
            d["hash"] = self.hash_hex
        if self.md5_hex:
            d["md5"] = self.md5_hex
        if self.mode is not None:
            d["mode"] = oct(self.mode)
        if self.mtime is not None:
            d["mtime"] = self.mtime
        if self.uid is not None:
            d["uid"] = self.uid
        if self.gid is not None:
            d["gid"] = self.gid
        if self.file_count is not None:
            d["file_count"] = self.file_count
        if self.dir_count is not None:
            d["dir_count"] = self.dir_count
        if include_children and self.children:
            d["children"] = [c.to_dict(include_children=True) for c in self.children]
        return d

    def __repr__(self) -> str:
        kind = self.kind.name[0]
        extra = f" {self.human_size}" if self.size is not None else ""
        kids = f" [{len(self.children)} children]" if self.children else ""
        return f"<TreeEntry {kind} {self.path!r}{extra}{kids}>"


# ---------------------------------------------------------------------------
# Index wrapper
# ---------------------------------------------------------------------------

@dataclass
class PtarIndex:
    """
    Top-level object returned by :func:`load_index`.

    Attributes
    ----------
    root : TreeEntry
        The root node of the tree.
    root_path : str
        The original filesystem path the index was built from.
    source_file : str
        Path of the file this index was loaded from.
    file_type : str
        ``"etr"`` for empty trees, ``"idx"`` for complete indexes,
        ``"unknown"`` otherwise.
    raw : Any
        The raw deserialized MessagePack object (for debugging).
    """

    root: TreeEntry
    root_path: str = ""
    source_file: str = ""
    file_type: str = "unknown"
    raw: Any = field(default=None, repr=False)

    # --- convenience accessors ---------------------------------------------

    @property
    def total_files(self) -> int:
        if self.root.file_count is not None:
            return self.root.file_count
        return sum(1 for _ in self.root.walk_files())

    @property
    def total_dirs(self) -> int:
        if self.root.dir_count is not None:
            return self.root.dir_count
        return sum(1 for _ in self.root.walk_dirs()) - 1  # exclude root

    @property
    def total_size(self) -> Optional[int]:
        return self.root.size

    @property
    def root_hash(self) -> Optional[str]:
        return self.root.hash_hex

    # --- delegated traversal -----------------------------------------------

    def walk(self) -> Generator[TreeEntry, None, None]:
        return self.root.walk()

    def walk_files(self) -> Generator[TreeEntry, None, None]:
        return self.root.walk_files()

    def walk_dirs(self) -> Generator[TreeEntry, None, None]:
        return self.root.walk_dirs()

    def find(
        self, predicate: Callable[[TreeEntry], bool]
    ) -> Generator[TreeEntry, None, None]:
        return self.root.find(predicate)

    def glob(self, pattern: str) -> Generator[TreeEntry, None, None]:
        return self.root.glob(pattern)

    def resolve(self, relpath: str) -> Optional[TreeEntry]:
        return self.root.resolve(relpath)

    # --- comparison --------------------------------------------------------

    def diff(self, other: "PtarIndex") -> "IndexDiff":
        """Compare this index against *other* (e.g. before / after)."""
        return diff_indexes(self, other)

    # --- export ------------------------------------------------------------

    def to_dict(self) -> dict:
        return {
            "root_path": self.root_path,
            "source_file": self.source_file,
            "file_type": self.file_type,
            "tree": self.root.to_dict(),
        }

    def to_json(self, **kwargs) -> str:
        import json

        kwargs.setdefault("indent", 2)
        kwargs.setdefault("ensure_ascii", False)
        return json.dumps(self.to_dict(), **kwargs)

    def to_flat_list(self) -> List[dict]:
        """
        Return a flat list of dicts (one per file), handy for loading into
        pandas or CSV export.
        """
        rows = []
        for e in self.root.walk_files():
            rows.append(
                {
                    "path": e.path,
                    "name": e.name,
                    "size": e.size,
                    "hash": e.hash_hex,
                    "md5": e.md5_hex,
                    "mode": e.mode,
                    "mtime": e.mtime,
                    "uid": e.uid,
                    "gid": e.gid,
                }
            )
        return rows

    def to_dataframe(self):
        """
        Return a pandas DataFrame of all file entries.

        Requires pandas to be installed.
        """
        import pandas as pd

        return pd.DataFrame(self.to_flat_list())

    # --- display -----------------------------------------------------------

    def print_tree(self, **kwargs) -> None:
        self.root.print_tree(**kwargs)

    def __repr__(self) -> str:
        ft = self.file_type.upper()
        tf = self.total_files
        td = self.total_dirs
        sz = _human_bytes(self.total_size) if self.total_size else "?"
        return (
            f"<PtarIndex [{ft}] {self.root_path!r}  "
            f"{tf} files, {td} dirs, {sz}>"
        )

    def __str__(self) -> str:
        lines = [repr(self)]
        if self.root_hash:
            lines.append(f"  root hash: {self.root_hash}")
        lines.append(f"  source:    {self.source_file}")
        return "\n".join(lines)


# ---------------------------------------------------------------------------
# Diff support
# ---------------------------------------------------------------------------

@dataclass
class ChangedEntry:
    path: str
    old: TreeEntry
    new: TreeEntry
    changes: List[str] = field(default_factory=list)

    def __repr__(self) -> str:
        return f"<Changed {self.path!r}: {', '.join(self.changes)}>"


@dataclass
class IndexDiff:
    """Result of comparing two PtarIndex objects."""

    added: List[TreeEntry] = field(default_factory=list)
    removed: List[TreeEntry] = field(default_factory=list)
    changed: List[ChangedEntry] = field(default_factory=list)

    @property
    def has_differences(self) -> bool:
        return bool(self.added or self.removed or self.changed)

    def summary(self) -> str:
        parts = []
        if self.added:
            parts.append(f"{len(self.added)} added")
        if self.removed:
            parts.append(f"{len(self.removed)} removed")
        if self.changed:
            parts.append(f"{len(self.changed)} changed")
        return ", ".join(parts) if parts else "no differences"

    def __repr__(self) -> str:
        return f"<IndexDiff: {self.summary()}>"


def diff_indexes(a: PtarIndex, b: PtarIndex) -> IndexDiff:
    """Compare two indexes by file path and metadata."""
    map_a: Dict[str, TreeEntry] = {e.path: e for e in a.walk_files()}
    map_b: Dict[str, TreeEntry] = {e.path: e for e in b.walk_files()}

    result = IndexDiff()

    for path, entry in map_b.items():
        if path not in map_a:
            result.added.append(entry)

    for path, entry in map_a.items():
        if path not in map_b:
            result.removed.append(entry)

    for path in set(map_a) & set(map_b):
        ea, eb = map_a[path], map_b[path]
        changes = []
        if ea.size != eb.size:
            changes.append(f"size {ea.size} → {eb.size}")
        if ea.hash_bytes != eb.hash_bytes and ea.hash_bytes and eb.hash_bytes:
            changes.append("hash changed")
        if ea.mode != eb.mode and ea.mode is not None and eb.mode is not None:
            changes.append(f"mode {oct(ea.mode)} → {oct(eb.mode)}")
        if changes:
            result.changed.append(ChangedEntry(path, ea, eb, changes))

    return result


# ---------------------------------------------------------------------------
# MessagePack → TreeEntry interpretation
# ---------------------------------------------------------------------------

def _interpret_tree(raw: Any) -> Tuple[TreeEntry, str]:
    """
    Interpret the raw msgpack object and return (root_entry, root_path).

    parallel-tar uses rmp-serde which serializes structs as arrays (positional)
    and enums as tagged dicts (``{"Variant": payload}``).

    The known node format is::

        [name, path, kind_enum, metadata, hash]

    where *kind_enum* is ``{"Directory": [children]}`` or ``{"File": null}``
    (or similar), *metadata* is ``[size, file_count, dir_count]``, and *hash*
    is a hex-encoded SHA-256 string.
    """
    if isinstance(raw, dict):
        return _interpret_map(raw)

    if isinstance(raw, (list, tuple)):
        # Try the known ptar node format first
        if _looks_like_ptar_node(raw):
            root = _node_from_ptar(raw, parent=None)
            _fixup_parents(root)
            return root, root.path

        return _interpret_array(raw)

    raise ValueError(
        f"Unexpected top-level msgpack type: {type(raw).__name__}. "
        f"Use load_raw() to inspect the binary structure."
    )


def _looks_like_ptar_node(arr: list) -> bool:
    """
    Check whether *arr* matches the ptar node layout:
    [name:str, path:str, kind:dict, metadata:list, hash:str]
    """
    if not isinstance(arr, (list, tuple)):
        return False
    if len(arr) < 3:
        return False
    if not isinstance(arr[0], str):
        return False
    if not isinstance(arr[1], str):
        return False
    # arr[2] should be a tagged-enum dict like {"Directory": ...}
    if isinstance(arr[2], dict):
        keys = {k if isinstance(k, str) else k.decode() for k in arr[2]}
        if keys & {"Directory", "File", "Symlink"}:
            return True
    # arr[2] could also be a bare string variant like "File"
    if isinstance(arr[2], str) and arr[2] in ("File", "Directory", "Symlink"):
        return True
    return False


def _node_from_ptar(arr: list, parent: Optional[TreeEntry] = None) -> TreeEntry:
    """
    Parse a node matching the Rust ``TreeNode`` struct::

        pub struct TreeNode {
            pub name: String,                          // [0]
            pub path: PathBuf,                         // [1]
            pub node_type: NodeType,                   // [2]
            pub metadata: RwLock<Option<NodeMetadata>>, // [3]
            pub hash: RwLock<Option<String>>,           // [4]
        }

    ``rmp-serde`` serializes this as a 5-element array.  The
    ``RwLock<Option<T>>`` wrappers serialize as the inner ``Option``
    (``null`` when ``None``, or the value when ``Some``).
    """
    entry = TreeEntry(parent=parent)
    entry._raw = arr

    # [0] name: String
    entry.name = arr[0] if isinstance(arr[0], str) else str(arr[0])

    # [1] path: PathBuf (serialized as String)
    entry.path = arr[1] if isinstance(arr[1], str) else str(arr[1])

    # [2] node_type: NodeType — tagged enum
    #     {"Directory": [children]} or {"File": null} / "File"
    kind_raw = arr[2] if len(arr) > 2 else None
    entry.kind, children_raw = _parse_ptar_kind(kind_raw)

    # recurse into children
    if children_raw is not None:
        for child_raw in children_raw:
            if _looks_like_ptar_node(child_raw):
                child = _node_from_ptar(child_raw, parent=entry)
            else:
                child = _node_from_obj(child_raw, parent=entry)
            entry.children.append(child)

    # [3] metadata: Option<NodeMetadata>
    #     None (null) in .etr files, or a struct (serialized as array)
    #     in .idx files.  Observed layout: [size, file_count, dir_count]
    if len(arr) > 3 and arr[3] is not None:
        meta = arr[3]
        if isinstance(meta, (list, tuple)):
            if len(meta) >= 1 and isinstance(meta[0], (int, float)):
                entry.size = int(meta[0])
            if len(meta) >= 2 and isinstance(meta[1], (int, float)):
                entry.file_count = int(meta[1])
            if len(meta) >= 3 and isinstance(meta[2], (int, float)):
                entry.dir_count = int(meta[2])
        elif isinstance(meta, dict):
            # NodeMetadata might serialize as a map with named fields
            meta = _normalize_keys(meta)
            entry.size = _extract_int(meta, "size", "total_size")
            entry.file_count = _extract_int(meta, "file_count", "files", "n_files")
            entry.dir_count = _extract_int(meta, "dir_count", "dirs", "n_dirs")
            entry.mode = _extract_int(meta, "mode", "permissions")
            entry.mtime = _extract_number(meta, "mtime", "modified")
            entry.uid = _extract_int(meta, "uid")
            entry.gid = _extract_int(meta, "gid")

    # [4] hash: Option<String>  (hex-encoded SHA-256)
    #     None (null) in .etr files, or a hex string in .idx files.
    if len(arr) > 4 and arr[4] is not None:
        h = arr[4]
        if isinstance(h, str) and h:
            entry.hash_str = h
            try:
                entry.hash_bytes = bytes.fromhex(h)
            except ValueError:
                # not valid hex — store as-is, hash_bytes stays None
                pass
        elif isinstance(h, (bytes, bytearray)) and h:
            entry.hash_bytes = bytes(h)

    return entry


def _parse_ptar_kind(val: Any) -> Tuple[EntryKind, Optional[list]]:
    """
    Parse a ptar kind enum value.

    Returns ``(kind, children_or_none)``.  For ``Directory`` the children list
    is extracted from the enum payload; for other variants it is ``None``.
    """
    if val is None:
        return EntryKind.UNKNOWN, None

    # Tagged enum dict: {"Directory": [children]} or {"File": null}
    if isinstance(val, dict):
        for k, v in val.items():
            tag = k if isinstance(k, str) else k.decode()
            tag_lower = tag.lower()
            if "dir" in tag_lower:
                children = None
                if isinstance(v, (list, tuple)):
                    # v might be [children_list] (single-element tuple wrapper)
                    # or the children list directly
                    if len(v) == 1 and isinstance(v[0], (list, tuple)):
                        children = v[0]
                    else:
                        children = v
                return EntryKind.DIRECTORY, children
            if "file" in tag_lower or "reg" in tag_lower:
                return EntryKind.FILE, None
            if "sym" in tag_lower or "link" in tag_lower:
                return EntryKind.SYMLINK, None
        return EntryKind.UNKNOWN, None

    # Bare string variant: "File", "Directory", "Symlink"
    if isinstance(val, str):
        s = val.lower()
        if "dir" in s:
            return EntryKind.DIRECTORY, None
        if "file" in s or "reg" in s:
            return EntryKind.FILE, None
        if "sym" in s or "link" in s:
            return EntryKind.SYMLINK, None

    # Integer discriminant
    if isinstance(val, int):
        mapping = {
            0: EntryKind.FILE,
            1: EntryKind.DIRECTORY,
            2: EntryKind.SYMLINK,
        }
        return mapping.get(val, EntryKind.UNKNOWN), None

    return EntryKind.UNKNOWN, None


def _interpret_map(obj: dict) -> Tuple[TreeEntry, str]:
    """Interpret a dict-based (named-field) serialization."""
    d = _normalize_keys(obj)

    root_path = _extract_str(d, "root_path", "root", "path", "base_path") or ""

    tree_data = None
    for key in ("tree", "entries", "root", "index", "nodes", "children"):
        if key in d:
            tree_data = d[key]
            break

    if tree_data is None:
        tree_data = d

    root = _node_from_obj(tree_data, parent=None)
    if not root.path and root_path:
        root.path = root_path
        root.name = PurePosixPath(root_path).name
    _fixup_parents(root)
    return root, root_path


def _interpret_array(arr: list) -> Tuple[TreeEntry, str]:
    """
    Interpret a list-based (positional) serialization — fallback for formats
    that don't match the known ptar node layout.
    """
    if len(arr) >= 2 and isinstance(arr[0], str) and "/" in arr[0]:
        root_path = arr[0]
        tree_data = arr[1]
        root = _node_from_obj(tree_data, parent=None)
        if not root.path:
            root.path = root_path
            root.name = PurePosixPath(root_path).name
        _fixup_parents(root)
        return root, root_path

    root = _node_from_obj(arr, parent=None)
    root_path = root.path
    _fixup_parents(root)
    return root, root_path


def _node_from_obj(obj: Any, parent: Optional[TreeEntry] = None) -> TreeEntry:
    """Recursively convert a raw msgpack object into a TreeEntry."""
    if isinstance(obj, (list, tuple)) and _looks_like_ptar_node(obj):
        return _node_from_ptar(obj, parent)
    if isinstance(obj, dict):
        return _node_from_dict(obj, parent)
    if isinstance(obj, (list, tuple)):
        return _node_from_list(obj, parent)
    entry = TreeEntry(name=str(obj), path=str(obj), parent=parent)
    entry._raw = obj
    return entry


def _node_from_dict(d: dict, parent: Optional[TreeEntry]) -> TreeEntry:
    """Build a TreeEntry from a dict with named fields."""
    d = _normalize_keys(d)
    entry = TreeEntry(parent=parent)
    entry._raw = d

    entry.name = _extract_str(d, "name", "file_name", "basename") or ""
    entry.path = _extract_str(d, "path", "full_path", "file_path", "abs_path") or ""
    if not entry.name and entry.path:
        entry.name = PurePosixPath(entry.path).name

    kind_val = d.get("kind") or d.get("entry_type") or d.get("type") or d.get("file_type")
    entry.kind = _parse_kind(kind_val, d)

    entry.size = _extract_int(d, "size", "file_size", "total_size", "cumulative_size")
    entry.mode = _extract_int(d, "mode", "permissions", "perm")
    entry.mtime = _extract_number(d, "mtime", "modified", "modification_time", "mod_time")
    entry.uid = _extract_int(d, "uid", "user_id", "owner")
    entry.gid = _extract_int(d, "gid", "group_id", "group")
    entry.file_count = _extract_int(d, "file_count", "files", "n_files", "num_files")
    entry.dir_count = _extract_int(d, "dir_count", "dirs", "directories", "n_dirs", "num_dirs")

    entry.hash_bytes = _extract_hash(d, "hash", "sha256", "sha2", "checksum", "digest")
    entry.md5_bytes = _extract_hash(d, "md5", "md5sum", "md5_hash")

    children_raw = (
        d.get("children") or d.get("entries") or d.get("contents") or d.get("nodes")
    )
    if isinstance(children_raw, (list, tuple)):
        for child_obj in children_raw:
            child = _node_from_obj(child_obj, parent=entry)
            entry.children.append(child)

    if entry.children and entry.kind == EntryKind.UNKNOWN:
        entry.kind = EntryKind.DIRECTORY

    return entry


def _node_from_list(arr: list, parent: Optional[TreeEntry]) -> TreeEntry:
    """
    Build a TreeEntry from a positional (array) serialization — fallback for
    arrays that don't match the known ptar node layout.
    """
    entry = TreeEntry(parent=parent)
    entry._raw = arr

    if not arr:
        return entry

    strings = [(i, v) for i, v in enumerate(arr) if isinstance(v, str)]
    ints = [(i, v) for i, v in enumerate(arr) if isinstance(v, int)]
    byte_vals = [(i, v) for i, v in enumerate(arr) if isinstance(v, (bytes, bytearray))]
    lists = [(i, v) for i, v in enumerate(arr) if isinstance(v, (list, tuple))]
    dicts = [(i, v) for i, v in enumerate(arr) if isinstance(v, dict)]

    # First string that looks like a path → path
    for i, s in strings:
        if "/" in s or s.startswith("."):
            entry.path = s
            entry.name = PurePosixPath(s).name
            break
    else:
        if strings:
            entry.path = strings[0][1]
            entry.name = PurePosixPath(strings[0][1]).name

    # Bytes → hashes (32 bytes = SHA-256, 16 bytes = MD5)
    for _, b in byte_vals:
        if len(b) == 32 and entry.hash_bytes is None:
            entry.hash_bytes = b
        elif len(b) == 16 and entry.md5_bytes is None:
            entry.md5_bytes = b
        elif entry.hash_bytes is None:
            entry.hash_bytes = b

    # Largest int → size
    unassigned_ints = [v for _, v in ints]
    if unassigned_ints:
        max_int = max(unassigned_ints)
        if max_int > 0:
            entry.size = max_int

    # Nested list of dicts/lists → children
    for _, lst in lists:
        if lst and isinstance(lst[0], (dict, list, tuple)):
            for child_obj in lst:
                child = _node_from_obj(child_obj, parent=entry)
                entry.children.append(child)
            break

    if not entry.children and dicts:
        for _, d in dicts:
            child = _node_from_obj(d, parent=entry)
            entry.children.append(child)

    # Determine kind
    if entry.children:
        entry.kind = EntryKind.DIRECTORY
    else:
        for _, s in strings:
            sl = s.lower()
            if sl in ("file", "regular", "reg"):
                entry.kind = EntryKind.FILE
                break
            elif sl in ("dir", "directory"):
                entry.kind = EntryKind.DIRECTORY
                break
            elif sl in ("symlink", "link"):
                entry.kind = EntryKind.SYMLINK
                break
        else:
            if entry.hash_bytes or (entry.size and entry.size > 0):
                entry.kind = EntryKind.FILE

    return entry


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _normalize_keys(d: dict) -> dict:
    out = {}
    for k, v in d.items():
        if isinstance(k, bytes):
            k = k.decode("utf-8", errors="replace")
        if isinstance(k, str):
            k = k.lower().replace("-", "_").strip()
        out[k] = v
    return out


def _extract_str(d: dict, *keys: str) -> Optional[str]:
    for k in keys:
        val = d.get(k)
        if isinstance(val, str) and val:
            return val
        if isinstance(val, bytes):
            return val.decode("utf-8", errors="replace")
    return None


def _extract_int(d: dict, *keys: str) -> Optional[int]:
    for k in keys:
        val = d.get(k)
        if isinstance(val, int):
            return val
        if isinstance(val, float):
            return int(val)
    return None


def _extract_number(d: dict, *keys: str) -> Optional[float]:
    for k in keys:
        val = d.get(k)
        if isinstance(val, (int, float)):
            return float(val)
    return None


def _extract_hash(d: dict, *keys: str) -> Optional[bytes]:
    for k in keys:
        val = d.get(k)
        if isinstance(val, (bytes, bytearray)) and val:
            return bytes(val)
        if isinstance(val, str) and val:
            try:
                return bytes.fromhex(val)
            except ValueError:
                pass
        if isinstance(val, (list, tuple)) and val and isinstance(val[0], int):
            try:
                return bytes(val)
            except (ValueError, OverflowError):
                pass
    return None


def _parse_kind(val: Any, d: dict) -> EntryKind:
    if val is None:
        if d.get("children") or d.get("entries"):
            return EntryKind.DIRECTORY
        return EntryKind.UNKNOWN

    if isinstance(val, str):
        s = val.lower()
        if s in ("file", "regular", "reg", "f"):
            return EntryKind.FILE
        if s in ("dir", "directory", "d"):
            return EntryKind.DIRECTORY
        if s in ("symlink", "link", "l"):
            return EntryKind.SYMLINK

    if isinstance(val, int):
        mapping = {0: EntryKind.FILE, 1: EntryKind.DIRECTORY, 2: EntryKind.SYMLINK}
        return mapping.get(val, EntryKind.UNKNOWN)

    if isinstance(val, dict):
        for k in val:
            kl = (k if isinstance(k, str) else k.decode()).lower()
            if "file" in kl or "reg" in kl:
                return EntryKind.FILE
            if "dir" in kl:
                return EntryKind.DIRECTORY
            if "sym" in kl or "link" in kl:
                return EntryKind.SYMLINK

    return EntryKind.UNKNOWN


def _fixup_parents(root: TreeEntry) -> None:
    """Recursively set parent pointers and infer paths."""
    for child in root.children:
        child.parent = root
        if not child.path and root.path and child.name:
            child.path = root.path.rstrip("/") + "/" + child.name
        _fixup_parents(child)


def _glob_match(path: str, pattern: str) -> bool:
    if "**" not in pattern:
        return fnmatch.fnmatch(path, pattern)

    import re

    # Build regex from glob: ** matches anything (including /),
    # * matches anything except /, ? matches one char except /
    regex = ""
    i = 0
    while i < len(pattern):
        if pattern[i : i + 2] == "**":
            regex += ".*"
            i += 2
            # consume a trailing / so **/ doesn't require a slash
            if i < len(pattern) and pattern[i] == "/":
                regex += "/?"
                i += 1
        elif pattern[i] == "*":
            regex += "[^/]*"
            i += 1
        elif pattern[i] == "?":
            regex += "[^/]"
            i += 1
        else:
            regex += re.escape(pattern[i])
            i += 1

    return bool(re.fullmatch(regex, path))


def _human_bytes(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB", "TB", "PB"):
        if abs(n) < 1024.0:
            if unit == "B":
                return f"{n} B"
            return f"{n:.2f} {unit}"
        n /= 1024.0
    return f"{n:.2f} EB"


# ---------------------------------------------------------------------------
# Public API: load an index
# ---------------------------------------------------------------------------

def load_index(
    path: Union[str, os.PathLike],
    *,
    debug: bool = False,
) -> PtarIndex:
    """
    Load a parallel-tar index file (``.etr`` or ``.idx``).

    Parameters
    ----------
    path : str or Path
        Path to the index file.
    debug : bool
        If True, print raw msgpack structure info to stderr.

    Returns
    -------
    PtarIndex
        A wrapper with the parsed tree and convenience methods.
    """
    path = os.fspath(path)
    raw = load_raw(path)

    if debug:
        import sys

        print(f"[ptar_index] Raw type: {type(raw).__name__}", file=sys.stderr)
        if isinstance(raw, dict):
            print(f"[ptar_index] Keys: {list(raw.keys())}", file=sys.stderr)
        elif isinstance(raw, (list, tuple)):
            print(f"[ptar_index] Length: {len(raw)}", file=sys.stderr)
            for i, item in enumerate(raw[:5]):
                t = type(item).__name__
                preview = repr(item)[:120]
                print(f"[ptar_index]   [{i}] {t}: {preview}", file=sys.stderr)

    root, root_path = _interpret_tree(raw)

    ext = os.path.splitext(path)[1].lower()
    file_type = {".etr": "etr", ".idx": "idx"}.get(ext, "unknown")

    return PtarIndex(
        root=root,
        root_path=root_path or root.path,
        source_file=path,
        file_type=file_type,
        raw=raw,
    )


# ---------------------------------------------------------------------------
# Diagnostic: dump the raw structure of an index file
# ---------------------------------------------------------------------------

def describe_raw(path: Union[str, os.PathLike], *, max_depth: int = 4) -> str:
    """
    Return a human-readable description of the raw msgpack structure.

    Useful for debugging format mismatches or adapting this module to new
    versions of parallel-tar.
    """
    raw = load_raw(path)
    lines: list[str] = []
    _describe(raw, lines, indent=0, max_depth=max_depth)
    return "\n".join(lines)


def _describe(obj: Any, lines: list[str], indent: int, max_depth: int) -> None:
    prefix = "  " * indent
    if max_depth <= 0:
        lines.append(f"{prefix}…")
        return

    if isinstance(obj, dict):
        lines.append(f"{prefix}dict ({len(obj)} keys)")
        for k, v in list(obj.items())[:20]:
            k_repr = repr(k)
            if isinstance(v, (dict, list, tuple)):
                lines.append(f"{prefix}  {k_repr} →")
                _describe(v, lines, indent + 2, max_depth - 1)
            elif isinstance(v, bytes):
                lines.append(f"{prefix}  {k_repr} → bytes[{len(v)}]")
            elif isinstance(v, str) and len(v) > 80:
                lines.append(f"{prefix}  {k_repr} → str[{len(v)}]: {v[:80]!r}…")
            else:
                lines.append(f"{prefix}  {k_repr} → {type(v).__name__}: {v!r}")
        if len(obj) > 20:
            lines.append(f"{prefix}  … ({len(obj) - 20} more keys)")

    elif isinstance(obj, (list, tuple)):
        tname = type(obj).__name__
        lines.append(f"{prefix}{tname}[{len(obj)}]")
        for i, item in enumerate(obj[:5]):
            lines.append(f"{prefix}  [{i}] →")
            _describe(item, lines, indent + 2, max_depth - 1)
        if len(obj) > 5:
            lines.append(f"{prefix}  … ({len(obj) - 5} more items)")

    elif isinstance(obj, bytes):
        lines.append(f"{prefix}bytes[{len(obj)}]: {obj[:32].hex()}")

    else:
        lines.append(f"{prefix}{type(obj).__name__}: {obj!r}")


# ---------------------------------------------------------------------------
# CLI entry point
# ---------------------------------------------------------------------------

def _cli() -> None:
    """Minimal CLI for quick inspection."""
    import argparse
    import sys

    p = argparse.ArgumentParser(
        description="Inspect parallel-tar index files (.etr / .idx)"
    )
    p.add_argument("file", help="Path to the .etr or .idx file")
    p.add_argument(
        "--raw",
        action="store_true",
        help="Dump the raw msgpack structure instead of the parsed tree",
    )
    p.add_argument(
        "--tree",
        action="store_true",
        help="Print the tree (default action)",
    )
    p.add_argument(
        "--depth",
        type=int,
        default=3,
        help="Max depth for tree display (default: 3)",
    )
    p.add_argument(
        "--json",
        action="store_true",
        help="Export the full tree as JSON",
    )
    p.add_argument(
        "--files",
        action="store_true",
        help="List all file entries (path + size + hash)",
    )
    p.add_argument(
        "--glob",
        type=str,
        default=None,
        help="Filter entries by glob pattern",
    )
    p.add_argument(
        "--debug",
        action="store_true",
        help="Print debug info about the raw msgpack structure",
    )

    args = p.parse_args()

    if args.raw:
        print(describe_raw(args.file, max_depth=args.depth))
        return

    idx = load_index(args.file, debug=args.debug)

    if args.json:
        print(idx.to_json())
        return

    if args.files:
        gen = idx.glob(args.glob) if args.glob else idx.walk_files()
        for e in gen:
            h = e.hash_hex[:16] if e.hash_hex else "—"
            sz = e.human_size
            print(f"{e.path}\t{sz}\t{h}")
        return

    if args.glob:
        for e in idx.glob(args.glob):
            kind = "D" if e.is_dir else "F"
            sz = e.human_size
            print(f"[{kind}] {e.path}\t{sz}")
        return

    # Default: summary + tree
    print(idx)
    print()
    idx.print_tree(max_depth=args.depth)


if __name__ == "__main__":
    _cli()
