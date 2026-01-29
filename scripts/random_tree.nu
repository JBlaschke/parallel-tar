#!/usr/bin/env nu
# random_tree.nu
#
# Create a random directory tree filled with random files & random contents.
# (Run: nu random_tree.nu --help)

def rand_count [avg: int] {
    if $avg <= 0 { 0 } else { random int 0..(2 * $avg) }
}

def rand_size_bytes [min_b: int, max_b: int] {
    if $max_b <= $min_b { $min_b } else { random int ($min_b)..($max_b) }
}

def write_random_file [file_path: path, size_b: int, text: bool] {
    if $text {
        # Random ASCII letters+digits (size_b chars) -> text file
        random chars -l $size_b | save -f $file_path
    } else {
        # Random bytes (size_b bytes) -> raw binary file
        random binary $size_b | save -r -f $file_path
    }
}

def gen_dir [
    dir: path
    depth: int
    avg_files: int
    avg_dirs: int
    min_b: int
    max_b: int
    text: bool
    verbose: bool
] {
    mkdir $dir
    if $verbose { print $"dir  ($dir)" }

    let ext = (if $text { "txt" } else { "bin" })

    let n_files = (rand_count $avg_files)
    for i in 0..<($n_files) {
        let size_b = (rand_size_bytes $min_b $max_b)
        let fname = $"file_($i)_((random chars -l 8)).($ext)"
        let fpath = ($dir | path join $fname)

        write_random_file $fpath $size_b $text
        # FIX: "bytes" must be outside the interpolation parentheses
        if $verbose { print $"file ($fpath)  ($size_b) bytes" }
    }

    if $depth > 0 {
        mut n_dirs = (rand_count $avg_dirs)
        if $n_dirs == 0 { $n_dirs = 1 }  # ensure we reach the requested depth

        for j in 0..<($n_dirs) {
            let dname = $"dir_($j)_((random chars -l 8))"
            let dpath = ($dir | path join $dname)
            gen_dir $dpath ($depth - 1) $avg_files $avg_dirs $min_b $max_b $text $verbose
        }
    }
}

# Create a random directory tree filled with random files and random contents.
#
# Controls tree depth, approximate branching, average files per folder, and
# file sizes. By default files contain random *binary* bytes; use --text for
# random ASCII text.
#
# Examples:
#       nu random_tree.nu ./scratch --depth 4 --avg-files 6 --avg-dirs 3 --file-size 8KiB --clean
#       nu random_tree.nu ./scratch --depth 3 --avg-files 4 --min-size 256B --max-size 8KiB --text --clean --verbose
def main [
    root: path = "random_tree"         # Root directory to create
    --depth (-d): int = 3              # Levels below root (0 => only root)
    --avg-files (-f): int = 5          # Mean files per directory (~uniform 0..2*avg)
    --avg-dirs (-b): int = 2           # Mean subdirs per directory (~uniform 0..2*avg)
    --file-size (-s): filesize = 4KiB  # Fixed size if --min-size/--max-size not provided
    --min-size: filesize               # Min per-file size (bytes), enables random sizing
    --max-size: filesize               # Max per-file size (bytes), enables random sizing
    --text                             # Write ASCII text instead of raw binary
    --clean                            # Delete root first if it already exists
    --verbose (-v)                     # Print created dirs/files and sizes
] {
    if $depth < 0 { error make { msg: "--depth must be >= 0" } }
    if $avg_files < 0 { error make { msg: "--avg-files must be >= 0" } }
    if $avg_dirs < 0 { error make { msg: "--avg-dirs must be >= 0" } }

    let root_path = ($root | path expand)

    if ($root_path | path exists) {
        if $clean {
            rm -r -f $root_path
        } else {
            error make {
                msg: $"Refusing to write into existing path: ($root_path). Use --clean or choose a new root."
            }
        }
    }

    # Resolve min/max size (bytes) from the flags:
    let min_size0 = (if $min_size == null {
        if $max_size == null { $file_size } else { $max_size }
    } else { $min_size })

    let max_size0 = (if $max_size == null {
        if $min_size == null { $file_size } else { $min_size }
    } else { $max_size })

    let min_b0 = ($min_size0 | into int)
    let max_b0 = ($max_size0 | into int)

    # Swap if user passed them reversed:
    let min_b = (if $min_b0 <= $max_b0 { $min_b0 } else { $max_b0 })
    let max_b = (if $min_b0 <= $max_b0 { $max_b0 } else { $min_b0 })

    gen_dir $root_path $depth $avg_files $avg_dirs $min_b $max_b $text $verbose
    print $"Done. Root: ($root_path)"
}
