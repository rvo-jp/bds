"""Small LevelDB adapter. Native opens are permitted only on independent copies."""

from contextlib import contextmanager

import leveldb

from .model import require_regular_tree


@contextmanager
def opened(path):
    from pathlib import Path

    resolved = path.resolve(strict=True)
    for protected in ("/var/lib/bds", "/opt/bedrock", "/etc/bds"):
        base = Path(protected).resolve()
        if resolved == base or base in resolved.parents:
            raise ValueError("Production and snapshot databases must never be opened")
    if path.is_symlink():
        raise ValueError("Database directory must not be a link")
    require_regular_tree(path)
    if not (path / "CURRENT").is_file():
        raise ValueError("Existing offline database required")
    db = leveldb.LevelDB(str(path), False)
    try:
        yield db
    finally:
        db.close()


def get(db, key):
    try:
        return db.get(key)
    except KeyError:
        return None


def prefix_items(db, prefix):
    # No printable-text assumption: Bedrock keys and actor IDs contain binary bytes.
    for key, value in db.iterate(prefix, prefix + b"\xff" * 32):
        if not key.startswith(prefix):
            break
        yield key, value
