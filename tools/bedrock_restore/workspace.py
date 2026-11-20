"""Seed an offline workspace from a completed snapshot, never from a live DB."""

import argparse
import hashlib
import json
import shutil
import stat
import time
from pathlib import Path

from .filesystem import file_hash
from .model import require_regular_tree


def seed(workspace, label, snapshot):
    if label not in ("source", "baseline"):
        raise ValueError("Label must be source or baseline")
    root = Path(workspace).resolve()
    source = Path(snapshot).resolve(strict=True)
    if (
        root == Path("/")
        or (source / "data").is_symlink()
        or (source / "snapshot.json").is_symlink()
    ):
        raise ValueError("Root/link paths cannot be used as snapshot workspace inputs")
    for forbidden in ("/var/lib/bds", "/opt/bedrock", "/etc/bds"):
        protected = Path(forbidden).resolve()
        if root == protected or protected in root.parents:
            raise ValueError("Workspace cannot be a production/backup tree")
    if root == source or source in root.parents or root in source.parents:
        raise ValueError("Snapshot and workspace must be disjoint")
    metadata_bytes = (source / "snapshot.json").read_bytes()
    metadata = json.loads(metadata_bytes)
    if (
        metadata.get("schema") != 1
        or not metadata.get("id")
        or not isinstance(metadata.get("files"), dict)
    ):
        raise ValueError("Completed schema-1 snapshot required")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    root.chmod(0o700)
    target = root / (label + "-copy")
    if target.exists() or target.is_symlink():
        raise ValueError("Workspace copy already exists; do not overwrite evidence")
    started = time.monotonic()

    def copy_regular(src, dst):
        if not stat.S_ISREG(Path(src).lstat().st_mode):
            raise ValueError("Snapshot contains a link/special file")
        return shutil.copy2(src, dst)

    # Reject directory links too; copytree must never follow them into a live world.
    for entry in (source / "data").rglob("*"):
        mode = entry.lstat().st_mode
        if not stat.S_ISREG(mode) and not stat.S_ISDIR(mode):
            raise ValueError("Snapshot contains a link/special file")
    shutil.copytree(source / "data", target, copy_function=copy_regular)
    require_regular_tree(target)
    actual = {str(p.relative_to(target)) for p in target.rglob("*") if p.is_file()}
    if actual != set(metadata["files"]):
        raise ValueError("Snapshot file membership differs")
    for name, expected in metadata["files"].items():
        relative = Path(name)
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("Unsafe manifest path")
        path = target / relative
        original = source / "data" / relative
        if path.stat().st_size != expected["bytes"]:
            raise ValueError("Snapshot size mismatch")
        digest = file_hash(path)
        if digest != expected["sha256"]:
            raise ValueError("Snapshot checksum mismatch")
        if (path.stat().st_dev, path.stat().st_ino) == (
            original.stat().st_dev,
            original.stat().st_ino,
        ):
            raise ValueError("Workspace shares a snapshot inode")
    (root / (label + "-snapshot.json")).write_bytes(metadata_bytes)
    proof = {
        "id": metadata["id"],
        "label": label,
        "verified_files": len(actual),
        "snapshot_sha256": hashlib.sha256(metadata_bytes).hexdigest(),
        "independent_inodes": True,
        "seconds": round(time.monotonic() - started, 2),
    }
    (root / (label + "-verified.json")).write_text(json.dumps(proof, indent=2))
    return proof


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workspace")
    parser.add_argument("label", choices=("source", "baseline"))
    parser.add_argument("snapshot")
    args = parser.parse_args()
    print(json.dumps(seed(args.workspace, args.label, args.snapshot)))
