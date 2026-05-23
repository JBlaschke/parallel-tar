# Parallel Tar
A set of multi-threaded archival tools: compress large data sets, and validate
their quality!

## Example workflow

For most large projects we recommend that you use the following 3-stage
workflow:

1. Generate an "empty" tree (which is essentially an index containing only the
   directory tree, and no checksums or metadata)
2. Using the empty tree as a reference, fill in tree meta data and checksums
   (generating a complete index)
3. Using the empty tree as a reference, generate an archive

(steps 2 and 3 and be done in parallel). Note that if files where removed
between steps 1 and 2 or 3, then you will see warnings; if files where added
between steps 1 and 2 or 3, then they will be ignored.

>[!TIP]
> This 3-step approach is not necessary if you just want to build an archive.
> Independently building a tree and an index is only relevant it you wish to
> ensure that the archive is byte-for-byte identical to the original data. Note
> that building an empty tree, _then_ creating an index saves time (as the
> process of walking the file system is not parallelised)

Using this approach will cause you to walk the directory tree only once --
which speeds up the archiving process for very large directory trees. Here are
detailed instructions on how to run each of the steps listed above. 

### Generating only the directory tree

Passing the `-e` flag to `parallel-idx` will make it generate an "empty" tree.
This is an index file containing only the directory tree, but leaving file
metadata (like size and checksum) empty. Note that the original root path of
where this directory tree is located is included in the `.etr` file.

In this example, we are generating an empty tree file (`example.etr`) from a
very large directory (`/global/projects/data`). Note that we can run this from
anywhere

```
$ parallel-idx -e -f example.etr /global/projects/data
[2026-02-21T18:20:52Z INFO  parallel_idx] Building tree for: '/global/projects/data'
[2026-02-21T19:18:55Z INFO  parallel_idx] Saving index: 'Idx("example.etr")'
```

### Compete index from directory tree

In this example, we are "filling in" the metadata from the directory tree. This
(primarily the hashes) will be used to ensure that the archive is correct when
extracted later, and to find where differences are located (if there are any). 

>[!IMPORTANT]
> Empty tree files (`.etr`) as well as "complete" index files (`.idx`) contain
> the full path to each file. If the index is generated using an absolute path
> as its inpute (like in this example), then the `.etr` will contain the full
> path.

Use the `-t` flag to tell `parallel-idx` to generate from an `.etr` file
(instead of walkting the directory tree), and the `-n` flag to specify how many
parallel threads (workers) to use.

```
$ parallel-idx -n 16 -f example.idx -t example.etr
[2026-02-22T10:42:20Z INFO  parallel_idx] Building tree for: 'example.etr'
[2026-02-22T10:42:20Z INFO  parallel_idx] Loading index at: 'Idx("example.etr")'
[2026-02-22T10:42:24Z INFO  parallel_idx] Using 16 threads...
[2026-02-22T10:42:24Z INFO  parallel_idx] Computing metadata ...
[2026-02-22T10:42:24Z INFO  parallel_idx] Computing hashes ...
[2026-02-22T11:11:37Z INFO  parallel_idx] Indexed: 4821995 files, 417285 directories, 1689.34 GB total
Root hash: '2ad978a95789be738d31cd9eac89519957f35d2f730a346c715e05f3355e70ab'
--- Largest Entries ------------------------------------------
0: /global/projects/data is 4821995 files + 417285 dirs (1689.34 GB, 2ad978a95789be73)
1: /global/projects/data/LCLS is 333 files + 164 dirs (616.24 GB, 80518e7e6b82b455)
2: /global/projects/data/LCLS/lw61.tar is 1 files + 0 dirs (306.68 GB, 95a67d4393e26c08)
3: /global/projects/data/LCLS/sit_psdm_data is 117 files + 18 dirs (306.68 GB, 0795fcc66957484a)
4: /global/projects/data/LCLS/sit_psdm_data/psdm is 117 files + 17 dirs (306.68 GB, c2b44466a502697a)
--------------------------------------------------------------
[2026-02-22T11:11:42Z INFO  parallel_idx] Saving index: 'Idx("example.idx")'
```

### Generate archive from directory tree

In this example, we are using the tree file (`example.etr`) to construct a
compressed archive from the paths located therein. Splitting up the process
into: 1. Generate tree; 2. Generate index; 3. Generate archive saves us from
having to walk the directory tree twice (to generate the index, and the
archive).

>[!IMPORTANT]
> Using empty tree files (`.etr`) to generate the archive means that if the
> target directory changes (or even is just moved to a new absolute path)
> between generating the tree and generating the archive, then `parallel-tar`
> will ignore those changes. Effectively ignoring new files, and files that
> cannot be found (because they have been (re)moved). In the later case you
> will see warnings being emitted. In the former case the failure will be
> silent.

> [!TIP]
> Use the `-z` flag to create (or extract) compressed archives.

Use the `-t` flag to tell `parallel-tar` to generate from an `.etr` file
(instead of walkting the directory tree), the `-c` flag to request archive
creation (as opposed to extraction, which is signaled using `-x`), `-f` where
the output is to go, and the `-n` flag to specify how many parallel threads
(workers) to use.

```
$ parallel-tar -n 16 -t -c -z -f example example.etr 
[2026-02-22T10:42:23Z INFO  ptar_lib::files::tree] Loading index at: 'Idx("example.etr")'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Setting current working dir to: '/global/projects/data'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Saving archive to: '/scratch/user/Archive/example'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] SETUP: Enumerating files. Following links? false
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] SETUP: Starting 16 worker threads
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 0 and writing to '/scratch/user/Archive/example/example.0.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 1 and writing to '/scratch/user/Archive/example/example.1.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 2 and writing to '/scratch/user/Archive/example/example.2.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 3 and writing to '/scratch/user/Archive/example/example.3.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 4 and writing to '/scratch/user/Archive/example/example.4.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 5 and writing to '/scratch/user/Archive/example/example.5.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 6 and writing to '/scratch/user/Archive/example/example.6.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 7 and writing to '/scratch/user/Archive/example/example.7.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 8 and writing to '/scratch/user/Archive/example/example.8.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 9 and writing to '/scratch/user/Archive/example/example.9.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 10 and writing to '/scratch/user/Archive/example/example.10.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 11 and writing to '/scratch/user/Archive/example/example.11.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 12 and writing to '/scratch/user/Archive/example/example.12.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 13 and writing to '/scratch/user/Archive/example/example.13.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 14 and writing to '/scratch/user/Archive/example/example.14.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Starting worker thread: 15 and writing to '/scratch/user/Archive/example/example.15.tar.gz'
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Sending paths to workers. This will start the archiving files...
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Collecting worker status (workers are working) ...
[2026-02-22T13:56:49Z INFO  ptar_lib::archive::tar]  ... waiting for workers to finish ...
[2026-02-22T13:56:49Z INFO  ptar_lib::archive::tar]  ... workers are done!
[2026-02-22T13:56:49Z INFO  ptar_lib::archive::tar] FINALIZE: checking worker status.
[2026-02-22T13:56:52Z INFO  ptar_lib::archive::tar] DONE.
```

You might not see much happening after
```
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Collecting worker status (workers are working) ...
```
This just means that the worker threads are hard at work. Once these threads
have completed, the list of requested and completed directories are compared.
This is what this line refers to:
```
[2026-02-22T13:56:49Z INFO  ptar_lib::archive::tar] FINALIZE: checking worker status.
```
If there are any files which were not acknowledged as compressed (by any of the
workers), then warnings are emitted. This will hapen if files are being removed
from the target location while workers are working.

>[!IMPORTANT]
> If given absolute paths, archives will always be "made from" the parent
> folder. Therefore the generated `.tar` files will always start at the
> enclosing folder (as if the tar process is always done within the parent
> folder). If given relative paths, then archives will be "made from" the root
> of the relative path (just like "regular" tar).

Note the line:
```
[2026-02-22T10:42:36Z INFO  ptar_lib::archive::tar] Setting current working dir to: '/global/projects/data'
```
is shows that `parallel-tar` is capable of identifying the parent directory of
the archive target, and switching to that location. This means that the current
working directory is switched to the parent of the tree root, and the achive
will usve the encloing folder (and only the enclosing folder) as its root. This
is distinct from "regular tar" where you must always specify a relative path.
Note that the "regular tar" behaviour is recovered when inputing relative
paths. For example if you request an archive of `a/b` then the resulting tar
will have `a/b` as its root; but if you request an archive of `/a/b` then the
resulting tar will have `b` as its root.

### Verify an archive against an index

Once you have an archive on disk, you'll want to confirm it actually
reconstructs the original tree. The `-i` (or `--verify`) flag scans a set of
`parallel-tar` shards in parallel and produces an index file with the same root
hash as the one `parallel-idx` would have generated from the live tree. This
means you can verify the archive end-to-end **without extracting it to disk**:

* Each worker thread opens one shard (e.g. `example.0.tar.gz`) and streams
  its entries through the same MD5 / SHA-256 routines used by
  `parallel-idx`.
* File contents are hashed incrementally — no temporary files, no full-file
  buffers.
* The resulting tree is assembled in memory and saved as an `.idx`.

The flags mirror the create/extract path: `-z` for compressed archives,
`-n` for thread count, `-f` for the archive directory.

```
$ parallel-tar -i -z -n 16 -f /scratch/user/Archive/example -o example.verify.idx
[2026-02-23T08:15:02Z INFO  ptar_lib::archive::verify] SETUP: Starting 16 worker threads to scan '/scratch/user/Archive/example'
[2026-02-23T08:15:02Z INFO  ptar_lib::archive::verify] Starting worker thread: 0 scanning '/scratch/user/Archive/example/example.0.tar.gz'
[2026-02-23T08:15:02Z INFO  ptar_lib::archive::verify] Starting worker thread: 1 scanning '/scratch/user/Archive/example/example.1.tar.gz'
...
[2026-02-23T11:32:18Z INFO  ptar_lib::archive::verify]  ... waiting for workers to finish scanning ...
[2026-02-23T11:32:18Z INFO  ptar_lib::archive::verify]  ... workers are done!
[2026-02-23T11:32:18Z INFO  ptar_lib::archive::verify] Collected 4821995 entries; assembling tree
Root hash: '2ad978a95789be738d31cd9eac89519957f35d2f730a346c715e05f3355e70ab'
[2026-02-23T11:32:24Z INFO  ptar_lib::archive::verify] Saving index: 'Idx("example.verify.idx")'
```

If the root hash printed here matches the one from your `example.idx` —
generated from the live tree — the archive is byte-for-byte sound.

>[!TIP]
> Use `-i` whenever you finish creating an archive that you'll move, upload, or
> store long-term. It's much cheaper than a full extract-and-rehash cycle, and
> it catches the same class of errors.

>[!IMPORTANT]
> Tar shards store paths relative to the parent of the original target (e.g.
> `data/LCLS/lw61.tar`), while a `.idx` built from an absolute path stores the
> full path (e.g. `/global/projects/data/LCLS/lw61.tar`). The **root hash** is
> independent of the path prefix and will match either way. For a byte-level
> comparison of the two `.idx` files (e.g. with `diff-idx`), pass `-r` to
> prepend the original prefix:
>
> ```
> $ parallel-tar -V -z -n 16 -f example -r /global/projects -o example.verify.idx
> ```
>
> Without `-r`, paths in the verify `.idx` will be tar-relative, which is still
> a valid index — root-hash comparison works either way.

### Compare two indexes with `diff-idx`

When two indexes' root hashes don't match, `diff-idx` shows you where they
diverge. By default it diffs at the root; pass `--path` (or `-p`) to focus on a
sub-tree. The output is one level deep — the requested node and its direct
children — so it stays readable even on enormous trees.

```
$ diff-idx example.idx example.verify.idx -p data/LCLS
Loading LEFT:  Idx("example.idx")
Loading RIGHT: Idx("example.verify.idx")
─────────────────────────────────────────────────────────────────────────────────────────
📁 LCLS (616.24 GB, 80518e7e6b82b455)   ==   📁 LCLS (616.24 GB, 80518e7e6b82b455)
─────────────────────────────────────────────────────────────────────────────────────────
📄 lw61.tar (306.68 GB, 95a67d4393e26c08)        ==   📄 lw61.tar (306.68 GB, 95a67d4393e26c08)
📄 metadata.json (1.2 KB, a1b2c3d4e5f60718)      !=   📄 metadata.json (1.2 KB, ff00112233445566)
📄 missing.dat (4.0 MB, 7788990011223344)         
                                                  >   📄 new_file.dat (4.0 MB, deadbeefcafef00d)
📁 sit_psdm_data (306.68 GB, 0795fcc66957484a)   ==   📁 sit_psdm_data (306.68 GB, 0795fcc66957484a)

Subtrees differ: MISMATCH
```

The middle column tells you what each row means:

| Marker | Meaning                                                  |
|--------|----------------------------------------------------------|
| `==`   | Both sides present, hashes match (rendered in green).    |
| `!=`   | Both sides present, hashes differ (rendered in red).     |
| `<`    | Entry exists only in the LEFT index.                     |
| `>`    | Entry exists only in the RIGHT index.                    |

Children are sorted by name on both sides and aligned by name — when an entry
is missing on one side, that side is left blank so the rest of the list stays
in step. You can re-run `diff-idx` recursively, drilling into whichever
differing directory you want to investigate next:

```
$ diff-idx example.idx example.verify.idx -p data/LCLS/sit_psdm_data
```

>[!TIP]
> `diff-idx` accepts both `.idx` (msgpack) and `.json` indexes — add `-j` when
> both files are JSON. The column width defaults to 40 chars per side; bump it
> with `-w 60` if you have long filenames.

>[!IMPORTANT]
> The `--path` argument is matched by **child name**, not by the
> `TreeNode.path` field. This means it works uniformly regardless of whether
> your indexes were built with absolute or relative paths — pass `data/LCLS`
> whether your indexes contain `/global/projects/data/LCLS` or just
> `data/LCLS`. If a path can't be resolved on either side, `diff-idx` errors
> out rather than silently diffing partial trees.

## Logging

The default log level is `info` -- if you would like more information, then
set: `RUST_LOG=debug`.

## License

`parallel-tar` is offered under a **dual-licensing** model. You may choose
**one** of the following licenses:

1. **Open Source License**: GNU Affero General Public License, version 3 or
   later  SPDX: `AGPL-3.0-or-later`  
   See: `LICENSE-AGPL` (and/or `LICENSE`)

2. **Commercial License**: A separate commercial license is available from
   Johannes Blaschke without the conditions of the GNU Affero GPL.
   See: `COMMERCIAL.md`

If you do not have a commercial license agreement with Johannes Blaschke, your
use of this project is governed by the **AGPL-3.0-or-later**.

### What this means (high level)

- The AGPL is an OSI-approved open-source license. You may use `parallel-tar`
  commercially under the AGPL if you comply with its terms.
- If you modify `parallel-tar` and run it to provide network access to users
  (e.g., as a service), the AGPL includes obligations related to offering the
  corresponding source code of the version you run.
- If your organization cannot or does not want to comply with the AGPL’s
  requirements, you can obtain a commercial license.

For commercial licensing inquiries: **Johannes Blaschke,
johannes@blaschke.science**

## Contributing

We welcome contributions!

To preserve the ability to offer `parallel-tar` under both open-source and
commercial licenses, all contributions must be made under the Contributor
License Agreement:

- See: `CLA.md`

By submitting a pull request (or otherwise contributing code), you agree that
your contribution is made under the terms of the CLA.

---

Copyright (c) 2026 Johannes Blaschke
