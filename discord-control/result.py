#!/usr/bin/env python3
"""Record a systemd invocation independently of unit garbage collection."""
import argparse
import json
import os
from pathlib import Path
import re
import tempfile


def write_result(directory, operation, phase, environment, boot):
    invocation = environment.get('INVOCATION_ID', '')
    if not re.fullmatch(r'[0-9a-f]{32}', invocation):
        raise ValueError('Missing systemd invocation ID')
    record = {'operation': operation, 'phase': phase, 'invocation_id': invocation,
              'boot_id': boot}
    if phase == 'finished':
        record.update(service_result=environment.get('SERVICE_RESULT', 'unknown'),
                      exit_code=environment.get('EXIT_CODE', ''),
                      exit_status=environment.get('EXIT_STATUS', ''))
    directory = Path(directory)
    descriptor, temporary = tempfile.mkstemp(prefix='.result-', dir=directory)
    try:
        with os.fdopen(descriptor, 'w') as stream:
            os.fchmod(stream.fileno(), 0o644)
            json.dump(record, stream)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, directory / 'result.json')
        descriptor = os.open(directory, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    finally:
        Path(temporary).unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('restart', 'reboot'))
    parser.add_argument('phase', choices=('running', 'finished'))
    args = parser.parse_args()
    write_result('/run/bds-control-' + args.operation, args.operation, args.phase,
                 os.environ, Path('/proc/sys/kernel/random/boot_id').read_text().strip())
