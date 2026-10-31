#!/usr/bin/env python3
"""BDS lifecycle and readiness. Linux/systemd; Python standard library only."""
import argparse
from collections import deque
import fcntl
import json
import os
from pathlib import Path
import re
import selectors
import signal
import socket
import subprocess
import sys
import time

STARTED = re.compile(r'^\[[^\]\n]*INFO\] Server started\.$')
LIST = re.compile(r'^\[[^\]\n]*INFO\] There are \d+/\d+ players online:')


def notify(message):
    address = os.environ.get('NOTIFY_SOCKET')
    if not address:
        return
    if address.startswith('@'):
        address = '\0' + address[1:]
    with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as sock:
        sock.sendto(message.encode(), address)


def atomic_json(path, data, mode=0o644):
    temp = path.with_name(path.name + '.tmp')
    fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
    with os.fdopen(fd, 'w') as stream:
        json.dump(data, stream, ensure_ascii=False)
        stream.write('\n')
    os.replace(temp, path)


def properties(runtime):
    result = {}
    for line in (runtime / 'server.properties').read_text().splitlines():
        if line.strip() and not line.lstrip().startswith('#') and '=' in line:
            key, value = line.split('=', 1)
            result[key.strip()] = value.strip()
    if result.get('transport', 'nethernet') != 'nethernet':
        raise ValueError('Supervision requires transport=nethernet')
    port = int(result.get('server-port', '19132'))
    if not 1 <= port <= 65535:
        raise ValueError('Invalid server-port')
    return port


def owns_listener(pid, port, proc=Path('/proc')):
    """Check the BDS child's own socket, without creating TCP/TIME_WAIT traffic."""
    try:
        sockets = set()
        for fd in (proc / str(pid) / 'fd').iterdir():
            try:
                link = os.readlink(fd)
            except FileNotFoundError:
                continue
            if link.startswith('socket:['):
                sockets.add(link[8:-1])
        for protocol in ('tcp', 'tcp6'):
            path = proc / str(pid) / 'net' / protocol
            if not path.exists():
                continue
            for line in path.read_text().splitlines()[1:]:
                fields = line.split()
                if (len(fields) >= 10 and fields[3] == '0A'
                        and int(fields[1].split(':')[1], 16) == port
                        and fields[9] in sockets):
                    return True
    except (OSError, ValueError):
        return False
    return False


class Supervisor:
    def __init__(self, args, *, clock=time.monotonic, listener=owns_listener, notifier=notify):
        self.args, self.clock, self.listener, self.notifier = args, clock, listener, notifier
        self.runtime = Path(args.runtime)
        self.state_dir = Path(args.state_dir)
        self.diagnostics = Path(args.diagnostics_dir)
        self.port = properties(self.runtime)
        self.logs = deque(maxlen=200)
        self.started = self.ready = self.stopping = self.failed = False
        self.reason = ''
        self.pending = self.last_reply = self.stable_since = None
        self.misses = 0
        self.child = None
        self.lock = self.fifo = None
        self.input_buffer = self.output_buffer = b''
        self.commands = bytearray()
        self.stop_requested = False
        self.next_check = 0
        self.next_publish = 0
        self.was_ready = False
        try:
            previous = json.loads((self.state_dir / 'status.json').read_text())
        except (OSError, ValueError):
            previous = {}
        self.recovered = bool(previous.get('failed'))

    def state(self):
        return {'invocation_id': os.environ.get('INVOCATION_ID', ''),
                'pid': self.child.pid if self.child else 0, 'ready': self.ready,
                'failed': self.failed, 'recovered': self.recovered,
                'port': self.port, 'checked_at': time.time(), 'reason': self.reason,
                'child_exit_status': self.child.poll() if self.child else None,
                'stopped': bool(self.child and self.child.poll() == 0 and self.stopping and not self.failed)}

    def publish(self):
        atomic_json(self.state_dir / 'status.json', self.state())

    def fail(self, reason):
        if self.failed:
            return
        self.failed, self.ready, self.reason = True, False, reason
        self.diagnostics.mkdir(parents=True, exist_ok=True)
        stamp = time.strftime('%Y%m%d-%H%M%S') + '-' + str(os.getpid())
        atomic_json(self.diagnostics / ('failure-' + stamp + '.json'),
                    {**self.state(), 'listener_owned': bool(self.child and self.listener(self.child.pid, self.port)),
                     'recent_output': list(self.logs)}, 0o600)
        for old in sorted(self.diagnostics.glob('failure-*.json'))[:-20]:
            old.unlink()
        self.publish()
        print('BDS supervisor: ' + reason, flush=True)
        self.stop()

    def stop(self):
        if self.stopping:
            return
        self.stopping, self.ready = True, False
        self.publish()
        self.notifier('STOPPING=1\nSTATUS=' + (self.reason or '正常停止中'))
        if self.child and self.child.poll() is None:
            self.send('stop')

    def send(self, command):
        payload = (command + '\n').encode()
        if len(payload) + len(self.commands) > 65536:
            print('BDS supervisor: コンソール送信キューが満杯です。', flush=True)
            return False
        self.commands.extend(payload)
        return self.flush_commands()

    def flush_commands(self):
        if not self.commands:
            return True
        try:
            written = self.child.stdin.write(self.commands)
            if written:
                del self.commands[:written]
            return True
        except BlockingIOError:
            return True  # Retry without blocking health checks or shutdown signals.
        except (BrokenPipeError, OSError):
            return False

    def line(self, raw):
        text = raw.decode('utf-8', errors='replace').rstrip('\r')
        print(text, flush=True)
        self.logs.append(text[:4096])
        if STARTED.fullmatch(text):
            self.started = True
        if LIST.match(text) and self.pending is not None:
            self.last_reply = self.clock()
            self.pending = None
            self.misses = 0

    def health(self, now):
        listening = self.listener(self.child.pid, self.port)
        timed_out = False
        if self.pending is not None and now - self.pending >= self.args.response_timeout:
            self.pending = None
            self.misses += 1
            timed_out = True
            self.stable_since = None
        if now >= self.next_check:
            self.next_check = now + (self.args.interval if self.ready else min(self.args.interval, 1))
            if not listening:
                self.misses += int(not timed_out)
                self.stable_since = None
            elif self.started and self.pending is None:
                self.pending = now
                if not self.send('list'):
                    self.pending = None
                    self.misses += 1
        responsive = (self.started and listening and self.last_reply is not None
                      and now - self.last_reply <= self.args.interval + self.args.response_timeout
                      and self.misses == 0)
        if not self.ready:
            if responsive:
                if self.stable_since is None:
                    self.stable_since = now
                if now - self.stable_since >= self.args.stable:
                    self.ready = self.was_ready = True
                    self.publish()
                    self.notifier('READY=1\nSTATUS=起動・TCP受付・コンソール応答を確認済み')
            else:
                self.stable_since = None
            if (self.stable_since is None and now >= self.start_deadline):
                self.fail('起動期限内にTCP受付とコンソール応答を確認できませんでした。')
        elif self.misses >= self.args.failures:
            self.fail('接続受付またはコンソール応答の異常が連続しました。')
        if now >= self.next_publish:
            self.publish()
            self.next_publish = now + 5

    def run(self):
        self.state_dir.mkdir(parents=True, exist_ok=True)
        self.lock = (self.runtime / '.supervisor.lock').open('a')
        fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        path = self.runtime / '.server.stdin'
        if path.exists():
            if not path.is_fifo():
                raise ValueError('Server input path is not a FIFO')
            path.unlink()
        os.mkfifo(path, 0o600)
        self.fifo = os.open(path, os.O_RDWR | os.O_NONBLOCK)
        env = dict(os.environ)
        env.pop('NOTIFY_SOCKET', None)
        def request_stop(*_):
            self.stop_requested = True
        for sig in (signal.SIGTERM, signal.SIGINT):
            signal.signal(sig, request_stop)
        self.child = subprocess.Popen(['./bedrock_server'], cwd=self.runtime, env=env,
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, bufsize=0)
        os.set_blocking(self.child.stdout.fileno(), False)
        os.set_blocking(self.child.stdin.fileno(), False)
        self.start_deadline = self.clock() + self.args.start_timeout
        self.publish()
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(self.fifo, selectors.EVENT_READ, 'input')
                selector.register(self.child.stdout, selectors.EVENT_READ, 'output')
                while True:
                    if self.stop_requested:
                        self.stop()
                    self.flush_commands()
                    for key, _ in selector.select(timeout=0.25):
                        data = os.read(key.fd, 65536)
                        if key.data == 'input':
                            self.input_buffer += data
                            while b'\n' in self.input_buffer:
                                command, self.input_buffer = self.input_buffer.split(b'\n', 1)
                                command = command.decode('utf-8', errors='replace')
                                if command.strip() == 'stop':
                                    self.stop_requested = True
                                    self.stop()
                                elif not self.stopping:
                                    self.send(command)
                            if len(self.input_buffer) > 65536:
                                self.input_buffer = b''
                        else:
                            self.output_buffer += data
                            while b'\n' in self.output_buffer:
                                line, self.output_buffer = self.output_buffer.split(b'\n', 1)
                                self.line(line)
                            if len(self.output_buffer) > 65536:
                                self.line(self.output_buffer[:65536])
                                self.output_buffer = b''
                            if not data:
                                selector.unregister(key.fileobj)
                    code = self.child.poll()
                    if code is not None:
                        if self.output_buffer:
                            self.line(self.output_buffer)
                        if not self.stopping:
                            self.fail('BDSが予期せず終了しました（終了コード ' + str(code) + '）。')
                        self.ready = False
                        self.publish()
                        return 1 if self.failed or code != 0 else 0
                    now = self.clock()
                    if not self.stopping and (not self.ready or now >= self.next_check
                            or (self.pending is not None and now - self.pending >= self.args.response_timeout)):
                        self.health(now)
                    if now >= self.next_publish:
                        self.publish()
                        self.next_publish = now + 5
        finally:
            # Never unlink another process's FIFO; singleton lock is held until exit.
            if self.child and self.child.poll() is not None:
                path.unlink(missing_ok=True)
            os.close(self.fifo)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime', required=True)
    parser.add_argument('--state-dir', required=True)
    parser.add_argument('--diagnostics-dir', required=True)
    parser.add_argument('--start-timeout', type=float, default=120)
    parser.add_argument('--stable', type=float, default=15)
    parser.add_argument('--interval', type=float, default=30)
    parser.add_argument('--response-timeout', type=float, default=10)
    parser.add_argument('--failures', type=int, default=3)
    args = parser.parse_args()
    if min(args.start_timeout, args.interval, args.response_timeout, args.failures) <= 0 or args.stable < 0:
        parser.error('Timeouts/interval/failures must be positive; stable must be nonnegative')
    supervisor = Supervisor(args)
    try:
        return supervisor.run()
    except Exception as error:
        print('BDS supervisor error: ' + type(error).__name__, file=sys.stderr, flush=True)
        # Keep supervising an existing child until normal shutdown completes. Do not orphan it.
        if supervisor.child and supervisor.child.poll() is None:
            try:
                supervisor.fail('監督処理で異常が発生しました。')
            except Exception:
                pass
            supervisor.send('stop')
            supervisor.child.wait()
            (supervisor.runtime / '.server.stdin').unlink(missing_ok=True)
        return 1


if __name__ == '__main__':
    sys.exit(main())
