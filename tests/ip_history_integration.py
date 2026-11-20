#!/usr/bin/env python3
"""Test optional UFW observation and log configuration without host mutations.

Linux/root with ufw and rsyslog required. Firewall/service commands are mocks;
the test rsyslog uses only a private Unix socket and temporary output files.
"""
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]


def execute(*args, env=None, **kwargs):
    return subprocess.run(args, capture_output=True, text=True, timeout=15,
                          env={**os.environ, **(env or {})}, **kwargs)


class HistoryIntegration(unittest.TestCase):
    def setUp(self):
        # Ubuntu's enforced rsyslog AppArmor profile permits config only here.
        # A hidden directory is excluded from the host's *.conf include.
        self.directory = tempfile.TemporaryDirectory(prefix='.bds-ip-integration-',
                                                     dir='/etc/rsyslog.d')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.events = self.root / 'events'
        self.env = {'PATH': f'{self.bin}:/usr/sbin:/usr/bin:/sbin:/bin',
                    'BDS_TEST_EVENTS': str(self.events)}
        self.program('nft', '''
printf 'nft %s\\n' "$*" >> "$BDS_TEST_EVENTS"
case "$*" in
 *bds-ipban.nft*) exit "${BDS_FAIL_BAN:-0}" ;;
 *bds-ip-history.nft*) exit "${BDS_FAIL_HISTORY:-0}" ;;
 --check*) exit "${BDS_FAIL_CHECK:-0}" ;;
esac
exit 0
''')
        self.program('logger', '''
printf 'logger\\n' >> "$BDS_TEST_EVENTS"
exit "${BDS_FAIL_LOGGER:-0}"
''')
        self.program('systemctl', 'printf "systemctl %s\\n" "$*" >> "$BDS_TEST_EVENTS"\n')

    def program(self, name, body):
        path = self.bin / name
        path.write_text('#!/bin/sh\nset -eu\n' + body)
        path.chmod(0o700)
        return path

    def ufw(self, *, reload=True, **env):
        # Use the installed UFW reload control flow, with network operations
        # replaced. No real nft, iptables, ufw or systemctl can run here.
        source = Path('/lib/ufw/ufw-init-functions').read_text()
        function = re.search(r'^ufw_reload\(\) \{.*?^\}', source, re.M | re.S)
        self.assertIsNotNone(function)
        fragment = (ROOT / 'deploy/ufw-bds.after-init.sh').read_text()
        fragment = fragment.replace('/usr/sbin/nft', str(self.bin / 'nft'))
        fragment = fragment.replace('/usr/bin/logger', str(self.bin / 'logger'))
        hook = self.root / 'after.init'
        hook.write_text(f'''#!/bin/sh
set -e
case "$1" in
start)
{fragment}
    ;;
stop) ;;
esac
''')
        hook.chmod(0o700)
        if not reload:
            return execute(str(hook), 'start', env={**self.env, **env})
        driver = '''
ufw_stop() { printf 'ufw_stop\\n' >> "$BDS_TEST_EVENTS"; }
ufw_start() {
    printf 'ufw_start\\n' >> "$BDS_TEST_EVENTS"
    "$RULES_PATH/after.init" start
}
''' + function.group(0) + '\nufw_reload\n'
        return execute('sh', '-c', driver,
                       env={**self.env, 'RULES_PATH': str(self.root),
                            'ENABLED': 'yes', **env})

    def test_ufw_success(self):
        result = self.ufw()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.events.read_text()
        self.assertIn('ufw_stop\n', calls)
        self.assertIn('ufw_start\n', calls)
        self.assertNotIn('logger\n', calls)

    def test_optional_observation_failure(self):
        for reload in (False, True):
            for logger_failure in ('0', '1'):
                with self.subTest(reload=reload, logger_failure=logger_failure):
                    self.events.unlink(missing_ok=True)
                    result = self.ufw(reload=reload, BDS_FAIL_HISTORY='1',
                                      BDS_FAIL_LOGGER=logger_failure)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn('Continuing UFW startup.', result.stderr)
                    self.assertIn('logger\n', self.events.read_text())
                    if reload:
                        self.assertIn('ufw_start\n', self.events.read_text())

    def test_required_ban_failure_is_not_hidden(self):
        for reload in (False, True):
            with self.subTest(reload=reload):
                self.events.unlink(missing_ok=True)
                result = self.ufw(reload=reload, BDS_FAIL_BAN='1')
                self.assertNotEqual(result.returncode, 0)
                calls = self.events.read_text()
                if reload:
                    self.assertIn('ufw_start\n', calls)
                self.assertNotIn('--file /etc/ufw/bds-ip-history.nft', calls)

    def config_update(self, *, invalid_filter=False, **env):
        # Execute the documented procedure itself, with all destinations moved
        # to a temporary tree and firewall/service commands replaced by mocks.
        etc = self.root / 'etc'
        for name in ('ufw', 'rsyslog.d'):
            (etc / name).mkdir(parents=True)
        runtime = self.root / 'run'
        runtime.mkdir()
        (etc / 'rsyslog.conf').write_text(
            'action(type="omfile" file="' + str(self.root / 'unused.log') + '")\n')
        deploy = self.root / 'deploy'
        deploy.mkdir()
        for name in ('ufw-ip-history.nft.example', 'rsyslog-bds-ip-history.conf'):
            (deploy / name).write_bytes((ROOT / 'deploy' / name).read_bytes())
        if invalid_filter:
            (deploy / 'rsyslog-bds-ip-history.conf').write_text('this is not rsyslog syntax {\n')
        history = etc / 'ufw/bds-ip-history.nft'
        filter_path = etc / 'rsyslog.d/19-bds-ip-history.conf'
        history.write_text('existing history config\n')
        filter_path.write_text('# existing filter config\n')
        doc = (ROOT / 'docs/ip-history.md').read_text()
        procedure = doc.split("sudo sh -eu <<'SH'\n", 1)[1].split('\nSH\n', 1)[0]
        destinations = {'/etc/ufw/': str(etc / 'ufw') + '/',
                        '/etc/rsyslog.d/': str(etc / 'rsyslog.d') + '/',
                        '/etc/rsyslog.conf': str(etc / 'rsyslog.conf'),
                        '/run/bds-ip-': str(runtime / 'bds-ip-')}
        # Replace once: our private root is itself below /etc/rsyslog.d.
        procedure = re.sub('|'.join(map(re.escape, destinations)),
                           lambda match: destinations[match.group(0)], procedure)
        paths = re.findall(r'''(?<![A-Za-z0-9_./-])/(?:etc|run)/[^\s'"<>]+''', procedure)
        self.assertTrue(paths)
        self.assertTrue(all(path.startswith(str(self.root) + '/') for path in paths), paths)
        result = execute('sh', '-eu', '-c', procedure, cwd=self.root,
                         env={**self.env, **env})
        self.assertEqual(list((etc / 'ufw').glob('.bds-*')), [])
        self.assertEqual(list((etc / 'rsyslog.d').glob('.bds-*')), [])
        self.assertEqual(list(runtime.iterdir()), [])
        return result, history, filter_path, deploy

    def test_validated_update(self):
        result, history, filter_path, deploy = self.config_update()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(history.read_bytes(), (deploy / 'ufw-ip-history.nft.example').read_bytes())
        self.assertEqual(filter_path.read_bytes(), (deploy / 'rsyslog-bds-ip-history.conf').read_bytes())
        calls = self.events.read_text()
        self.assertLess(calls.index('nft --check'), calls.index('systemctl restart rsyslog'))

    def test_failed_nft_validation_keeps_both_originals(self):
        result, history, filter_path, _ = self.config_update(BDS_FAIL_CHECK='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(history.read_text(), 'existing history config\n')
        self.assertEqual(filter_path.read_text(), '# existing filter config\n')
        self.assertNotIn('systemctl', self.events.read_text())

    def test_failed_rsyslog_validation_keeps_both_originals(self):
        result, history, filter_path, _ = self.config_update(invalid_filter=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('Permission denied', result.stderr)
        self.assertEqual(history.read_text(), 'existing history config\n')
        self.assertEqual(filter_path.read_text(), '# existing filter config\n')
        self.assertNotIn('systemctl', self.events.read_text())

    def test_documented_ban_update(self):
        self.program('sudo', 'exec "$@"\n')
        self.program('sudoedit', 'printf "changed\\n" > "$1"\n')
        self.program('ufw', 'exit 0\n')
        doc = (ROOT / 'docs/firewall.md').read_text()
        procedure = re.search(r'```sh\n(candidate=\$\(sudo mktemp .*?)\n```', doc, re.S).group(1)
        for failure in ('0', '1'):
            with self.subTest(validation_failure=failure):
                self.events.unlink(missing_ok=True)
                private = self.root / ('ufw-' + failure)
                private.mkdir()
                target = private / 'bds-ipban.nft'
                target.write_text('existing ban config\n')
                script = procedure.replace('/etc/ufw/', str(private) + '/')
                result = execute('sh', '-c', script,
                                 env={**self.env, 'BDS_FAIL_CHECK': failure})
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = self.events.read_text()
                if failure == '0':
                    self.assertEqual(target.read_text(), 'changed\n')
                    self.assertEqual(target.stat().st_mode & 0o777, 0o600)
                    self.assertIn('nft --file', calls)
                else:
                    self.assertEqual(target.read_text(), 'existing ban config\n')
                    self.assertNotIn('nft --file', calls)
                self.assertEqual(list(private.glob('.bds-*')), [])

    def test_rsyslog_filter_preserves_other_logs(self):
        # Use paths permitted by the existing profile, without weakening it.
        socket_dir = tempfile.TemporaryDirectory(prefix='bds-ip-integration-', dir='/var/lib')
        self.addCleanup(socket_dir.cleanup)
        log_dir = tempfile.TemporaryDirectory(prefix='.bds-ip-integration-', dir='/var/log')
        self.addCleanup(log_dir.cleanup)
        (Path(socket_dir.name) / 'dev').mkdir()
        address = Path(socket_dir.name) / 'dev/log'
        output = Path(log_dir.name) / 'output.log'
        filter_path = self.root / 'filter.conf'
        filter_path.write_bytes((ROOT / 'deploy/rsyslog-bds-ip-history.conf').read_bytes())
        config = self.root / 'rsyslog.conf'
        config.write_text(f'''module(load="imuxsock" SysSock.Use="off")
input(type="imuxsock" Socket="{address}")
include(file="{filter_path}")
action(type="omfile" file="{output}")
''')
        checked = execute('rsyslogd', '-N1', '-f', str(config))
        self.assertEqual(checked.returncode, 0, checked.stderr)
        with (self.root / 'rsyslog.stderr').open('w+') as errors:
            environment = {k: v for k, v in os.environ.items()
                           if k not in {'NOTIFY_SOCKET', 'LISTEN_FDS', 'LISTEN_PID', 'LISTEN_FDNAMES'}}
            process = subprocess.Popen(['rsyslogd', '-n', '-iNONE', '-f', str(config)],
                                       stdout=subprocess.DEVNULL, stderr=errors, env=environment)
            try:
                deadline = time.monotonic() + 5
                while not address.exists() and time.monotonic() < deadline and process.poll() is None:
                    time.sleep(.02)
                self.assertTrue(address.exists(), 'private rsyslog socket did not start')
                with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as sender:
                    for message in ('<6>audit: [BDS-IP] private-history',
                                    '<6>audit: ordinary-kernel',
                                    '<14>audit: [BDS-IP] ordinary-user'):
                        sender.sendto(message.encode(), str(address))
                content = ''
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    content = output.read_text() if output.exists() else ''
                    if 'ordinary-kernel' in content and 'ordinary-user' in content:
                        break
                    time.sleep(.02)
                self.assertIn('ordinary-kernel', content)
                self.assertIn('ordinary-user', content)
                self.assertNotIn('private-history', content)
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            errors.seek(0)
            self.assertEqual(process.returncode, 0, errors.read())


if __name__ == '__main__':
    if sys.platform != 'linux' or os.geteuid() != 0:
        raise RuntimeError('requires Linux root for isolated ownership checks')
    unittest.main(verbosity=2)
