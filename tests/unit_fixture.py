"""Keep generated directory ownership and failure hooks inside a test fixture."""
from pathlib import Path
import shlex


def isolated_unit(text, root):
    root = Path(root).resolve()
    lines = []
    for line in text.splitlines():
        key, separator, value = line.partition('=')
        key = key.strip()
        if separator and key == 'OnFailure':
            continue
        lines.append(line)
        if not separator or key not in ('RuntimeDirectory', 'StateDirectory'):
            continue
        base = Path('/run' if key == 'RuntimeDirectory' else '/var/lib')
        for name in shlex.split(value):
            relative = Path(name)
            if (':' in name or relative.is_absolute() or '..' in relative.parts
                    or not (base / relative).resolve().is_relative_to(root)):
                raise RuntimeError(f'{key} escapes test fixture: {name}')
    return '\n'.join(lines) + '\n'
