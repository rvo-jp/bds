#!/usr/bin/env python3
"""Exercise IP observation in disposable Linux networks, never the host rules.

Usage (root): python3 tests/ip_history_acceptance.py HISTORY_CONFIG BAN_CONFIG
Requires nft >= 1.0.9, ip and unshare. No production journal settings are changed.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time
import uuid


def run(*args, check=True, data=None, timeout=15):
    result = subprocess.run(list(map(str, args)), input=data, capture_output=True,
                            text=True, timeout=timeout)
    if check and result.returncode:
        raise AssertionError(result.stdout + result.stderr)
    return result


def apply(text):
    run('nft', '--check', '-f', '-', data=text)
    run('nft', '-f', '-', data=text)


def table():
    return json.loads(run('nft', '-j', 'list', 'table', 'inet',
                          'bds_ip_history').stdout)['nftables']


def count(name):
    return next(item['counter']['packets'] for item in table()
                if item.get('counter', {}).get('name') == name)


def main(history, bans):
    if sys.platform != 'linux' or os.geteuid() != 0:
        raise RuntimeError('requires Linux root')
    if os.readlink('/proc/self/ns/net') == os.readlink('/proc/1/ns/net'):
        raise RuntimeError('refuse the host network namespace')
    text = history.read_text()
    namespace = 'bds-ip-test-' + uuid.uuid4().hex[:10]
    server4, server6 = '198.18.0.1', 'fd00:bd5::1'
    client4, client6 = '198.18.0.2', 'fd00:bd5::2'
    sockets = []

    def send(source, protocol='udp', port=19132, blocked=False):
        # This process runs in the other namespace. A real veth ingress is
        # essential: loopback traffic must be excluded by the observer.
        result = run('ip', 'netns', 'exec', namespace, sys.executable, '-c', '''
import socket,sys
source,dest,protocol,port = sys.argv[1:]
family = socket.AF_INET6 if ':' in source else socket.AF_INET
with socket.socket(family, socket.SOCK_STREAM if protocol=='tcp' else socket.SOCK_DGRAM) as s:
    s.settimeout(.3)
    s.bind((source,0))
    s.connect((dest,int(port)))
    s.send(b'x')
''', source, server6 if ':' in source else server4, protocol, port,
                     check=not blocked)
        if blocked and protocol == 'tcp':
            assert result.returncode != 0, 'denied TCP connection succeeded'

    def listener(address, protocol='udp', port=19132):
        sock = socket.socket(socket.AF_INET6 if ':' in address else socket.AF_INET,
                             socket.SOCK_STREAM if protocol == 'tcp' else socket.SOCK_DGRAM)
        sockets.append(sock)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        if ':' in address:
            sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        sock.bind((address, port))
        sock.settimeout(.3)
        if protocol == 'tcp':
            sock.listen()
        return sock

    def delivered(sock, protocol='udp'):
        if protocol == 'tcp':
            peer, _ = sock.accept()
            with peer:
                peer.settimeout(.3)
                assert peer.recv(1) == b'x'
        else:
            assert sock.recv(1) == b'x'

    def empty(sock):
        try:
            sock.recv(1)
        except TimeoutError:
            return
        raise AssertionError('denied UDP packet arrived')

    run('ip', 'netns', 'add', namespace)
    try:
        run('ip', 'link', 'set', 'lo', 'up')
        run('ip', 'link', 'add', 'server0', 'type', 'veth', 'peer', 'name', 'client0')
        run('ip', 'link', 'set', 'client0', 'netns', namespace)
        run('ip', 'addr', 'add', server4 + '/24', 'dev', 'server0')
        run('ip', '-6', 'addr', 'add', server6 + '/64', 'dev', 'server0', 'nodad')
        run('ip', 'link', 'set', 'server0', 'up')
        run('ip', 'route', 'add', '192.0.2.0/24', 'dev', 'server0')
        run('ip', '-n', namespace, 'link', 'set', 'lo', 'up')
        run('ip', '-n', namespace, 'addr', 'add', client4 + '/24', 'dev', 'client0')
        run('ip', '-n', namespace, '-6', 'addr', 'add', client6 + '/64', 'dev', 'client0', 'nodad')
        run('ip', '-n', namespace, 'addr', 'add', '192.0.2.10/32', 'dev', 'client0')
        run('ip', '-n', namespace, 'link', 'set', 'client0', 'up')
        run('ip', '-n', namespace, 'route', 'add', 'default', 'via', server4)
        # tcp/udp may share a port. Both address families must be tested.
        peers = {(ip, proto): listener(ip, proto)
                 for ip in (server4, server6) for proto in ('tcp', 'udp')}
        apply(text)
        for (ip, protocol), sock in peers.items():
            source = client6 if ':' in ip else client4
            send(source, protocol)
            delivered(sock, protocol)
            send(source, protocol)
            delivered(sock, protocol)
        assert count('log_attempted') == 4
        assert count('candidates') == 4
        print('PASS IPv4/IPv6, separate TCP/UDP keys, duplicate suppression and delivery', flush=True)

        # Non-game ports and local probes must not produce history.
        web = listener(server4, 'tcp', 443)
        send(client4, 'tcp', 443)
        delivered(web, 'tcp')
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as local:
            local.sendto(b'x', (server4, 19132))
        delivered(peers[server4, 'udp'])
        assert count('log_attempted') == 4
        print('PASS management traffic and loopback probes are excluded', flush=True)

        # Shorten only the private fixture. A duplicate near expiry must not
        # refresh the first observation's TTL.
        apply(text.replace('timeout 5m', 'timeout 2s').replace('gc-interval 1m', 'gc-interval 100ms'))
        send(client4)
        delivered(peers[server4, 'udp'])
        time.sleep(1.3)
        send(client4)
        delivered(peers[server4, 'udp'])
        assert count('log_attempted') == 1
        time.sleep(.9)
        send(client4)
        delivered(peers[server4, 'udp'])
        assert count('log_attempted') == 2
        print('PASS expiry starts at first observation and allows a later record', flush=True)

        # All families/protocols share a single budget; use a slow refill and
        # one token so the test is independent of scheduler speed.
        apply(text.replace('5/second burst 50', '1/hour burst 1'))
        for (ip, protocol), sock in peers.items():
            send(client6 if ':' in ip else client4, protocol)
            delivered(sock, protocol)
        assert count('log_attempted') == 1
        assert count('rate_limited') >= 3
        assert count('set_add_failed') == 0
        print('PASS shared rate limit skips logging, preserves both protocols/families', flush=True)

        apply(text.replace('size 4096', 'size 1'))
        for (ip, protocol), sock in peers.items():
            send(client6 if ':' in ip else client4, protocol)
            delivered(sock, protocol)
        assert count('log_attempted') == 2
        assert count('set_add_failed') >= 2
        print('PASS full address sets count missing records without dropping traffic', flush=True)

        # Simulate the later UFW decision on the same game port. Observation
        # must neither bypass it nor miss a denied attempt.
        apply(text)
        for protocol in ('tcp', 'udp'):
            send('192.0.2.10', protocol)
            delivered(peers[server4, protocol], protocol)
        apply(bans.read_text())
        # Reset only observation state after proving the banned fixture can
        # reach the server, so routing failures cannot masquerade as a BAN.
        apply(text)
        before = count('log_attempted')
        send('192.0.2.10', 'tcp', blocked=True)
        send('192.0.2.10', 'udp', blocked=True)
        empty(peers[server4, 'udp'])
        assert count('log_attempted') == before + 2, table()
        send(client4)
        delivered(peers[server4, 'udp'])
        print('PASS IPBAN still denies observed attempts and permits other clients', flush=True)

        apply(text)
        apply('''table inet later_ufw {
 chain input {
  type filter hook input priority 0; policy accept;
  ip6 saddr fd00:bd5::2 meta l4proto { tcp, udp } th dport 19132 drop
 }
}''')
        send(client6, 'tcp', blocked=True)
        send(client6, 'udp', blocked=True)
        empty(peers[server6, 'udp'])
        assert count('log_attempted') == 2
        print('PASS observation does not bypass a later UFW deny for IPv6', flush=True)

        for _ in range(2):
            apply(text)
        rules = table()
        assert sum('chain' in item for item in rules) == 2
        assert sum('set' in item for item in rules) == 2
        assert next(item['chain']['prio'] for item in rules
                    if item.get('chain', {}).get('name') == 'input') == -20
        invalid = run('nft', '-f', '-', check=False, data=text + '\nadd rule inet bds_ip_history missing_chain drop\n')
        assert invalid.returncode != 0
        assert table() == rules, 'invalid transaction changed the live table'
        run('nft', 'destroy', 'table', 'inet', 'bds_ip_history')
        send('192.0.2.10', 'tcp', blocked=True)
        send('192.0.2.10', 'udp', blocked=True)
        empty(peers[server4, 'udp'])
        send(client6, 'tcp', blocked=True)
        send(client6, 'udp', blocked=True)
        empty(peers[server6, 'udp'])
        send(client4)
        delivered(peers[server4, 'udp'])
        print('PASS atomic reload, invalid config rollback and independent removal', flush=True)
    finally:
        for sock in sockets:
            sock.close()
        run('ip', 'netns', 'del', namespace)


if __name__ == '__main__':
    if not __debug__:
        raise RuntimeError('requires Python assertions enabled')
    if sys.platform != 'linux' or os.geteuid() != 0:
        raise RuntimeError('requires Linux root')
    paths = [str(Path(arg).resolve()) for arg in sys.argv[1:3]]
    if len(paths) != 2:
        raise RuntimeError('provide history and IPBAN configuration files')
    if '--inside' in sys.argv:
        main(*map(Path, paths))
    else:
        result = run('unshare', '--net', sys.executable, Path(__file__).resolve(), *paths, '--inside', timeout=60)
        print(result.stdout, end='')
        print('PASS all isolated IP history checks')
