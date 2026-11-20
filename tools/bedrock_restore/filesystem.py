"""Filesystem durability and exclusion for independent offline workspaces."""

import fcntl
import hashlib
import os
from contextlib import contextmanager


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def sync_tree(root):
    directories = [root]
    for path in root.rglob("*"):
        if path.is_symlink():
            raise ValueError("Cannot synchronize a linked workspace entry")
        if path.is_dir():
            directories.append(path)
        elif path.is_file():
            with path.open("rb") as handle:
                os.fsync(handle.fileno())
        else:
            raise ValueError("Cannot synchronize a non-regular workspace tree")
    for path in sorted(directories, key=lambda p: len(p.parts), reverse=True):
        descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)


@contextmanager
def workspace_lock(root):
    descriptor = os.open(
        root / ".restore.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600
    )
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield
    finally:
        os.close(descriptor)
