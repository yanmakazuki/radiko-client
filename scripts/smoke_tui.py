#!/usr/bin/env python3
"""PTY smoke test for `tui --offline`. No radiko, account, or audio device.

The child receives a synthetic catalog only. Screen text, terminal bytes, and
the child stay in memory. A failure names the check, not the terminal contents.
"""
import argparse
import os
from pathlib import Path
import pty
import select
import struct
import sys
import subprocess
import tempfile
import termios
import time

from verify_tui_playback import Terminal, report, require


MARKERS = ('ALSA lib', 'panicked')


def output_problems(data):
    """Return forbidden fragments. ASCII markers survive a partial UTF-8 tail."""
    text = data.decode('utf-8', 'replace')
    return [marker for marker in MARKERS if marker in text]


def network_sockets(pid):
    """Return TCP/UDP sockets owned by one process. None where /proc is unavailable.

    Anonymous Unix sockets are local wakeups (the runtime and the terminal), not radiko.
    """
    fd_dir = '/proc/%s/fd' % pid
    if not os.path.isdir(fd_dir):
        return None
    owned = set()
    for name in os.listdir(fd_dir):
        try:
            target = os.readlink(os.path.join(fd_dir, name))
        except OSError:
            continue
        if target.startswith('socket:[') and target.endswith(']'):
            owned.add(target[len('socket:['):-1])
    if not owned:
        return []
    found = []
    for path in ('/proc/net/tcp', '/proc/net/tcp6', '/proc/net/udp', '/proc/net/udp6'):
        try:
            with open(path, encoding='utf-8') as handle:
                lines = handle.read().splitlines()
        except OSError:
            continue
        for line in lines[1:]:
            fields = line.split()
            if len(fields) > 9 and fields[9] in owned:
                found.append('%s %s' % (os.path.basename(path), fields[1]))
    return found


def child_env(directory):
    return {
        'PATH': os.environ.get('PATH', '/usr/bin'),
        'HOME': directory,
        'TMPDIR': directory,
        'LANG': 'C.UTF-8',
        'LC_ALL': 'C.UTF-8',
        'TERM': 'xterm-256color',
        'XDG_DATA_HOME': directory,
        'XDG_CONFIG_HOME': directory,
        'XDG_CACHE_HOME': directory,
        'NO_PROXY': '*',
    }


class Capture:
    def __init__(self, master):
        self.master = master
        self.terminal = Terminal(master)
        self.raw = bytearray()

    def pump(self, seconds):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            timeout = min(0.05, max(0, until - time.monotonic()))
            if select.select([self.master], [], [], timeout)[0]:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                self.raw.extend(chunk)
                self.terminal.feed(chunk)

    def screen(self):
        return self.terminal.screen()


def wait_until(capture, proc, predicate, deadline, message):
    while time.monotonic() < deadline:
        capture.pump(0.05)
        require(proc.poll() is None, 'TUI exited before verification completed')
        require(not output_problems(capture.raw), 'forbidden text in TUI output')
        sockets = network_sockets(proc.pid)
        require(not sockets, 'TUI opened a network socket')
        if predicate(capture.screen()):
            return
    raise RuntimeError(message)


def wait_exit(proc, capture, deadline):
    while proc.poll() is None and time.monotonic() < deadline:
        capture.pump(0.05)
        require(not output_problems(capture.raw), 'forbidden text in TUI output')
    capture.pump(0.2)
    require(proc.poll() is not None, 'TUI process exit timed out')


def verify(binary, directory, rows, columns):
    os.chmod(directory, 0o700)
    Terminal.rows, Terminal.columns = rows, columns
    master, slave = pty.openpty()
    fcntl_winsize(slave, rows, columns)
    before = termios.tcgetattr(slave)
    capture = Capture(master)
    proc = None
    try:
        proc = subprocess.Popen(
            [str(binary), '--data-dir', directory, 'tui', '--offline', 'TBS'],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
            env=child_env(directory),
        )
        deadline = time.monotonic() + 45

        def ready(text):
            return ('番組一覧' in text and '取得完了' in text and 'オフライン番組' in text
                    and 'オフライン確認' in text and 'タイムフリー' in text)

        wait_until(capture, proc, ready, deadline, 'offline program list did not appear')
        os.write(master, b'?')
        wait_until(capture, proc, lambda text: '┌操作一覧' in text, deadline, 'help screen did not open')
        os.write(master, b'?')
        wait_until(
            capture, proc,
            lambda text: '┌操作一覧' not in text and '番組一覧' in text,
            deadline, 'help screen did not close',
        )
        os.write(master, b'f')
        wait_until(
            capture, proc,
            lambda text: '検索条件編集中' in text,
            deadline, 'filter screen did not open',
        )
        os.write(master, b'\x1b')
        wait_until(
            capture, proc,
            lambda text: '検索条件編集中' not in text and '番組一覧' in text,
            deadline, 'filter screen did not close',
        )
        os.write(master, b'e')
        wait_until(
            capture, proc,
            lambda text: '┌取得状況・未取得範囲' in text,
            deadline, 'failure screen did not open',
        )
        os.write(master, b'e')
        wait_until(
            capture, proc,
            lambda text: '┌取得状況・未取得範囲' not in text and '番組一覧' in text,
            deadline, 'failure screen did not close',
        )
        os.write(master, b'v')
        wait_until(
            capture, proc,
            lambda text: 'ライブ（' in text and '取得完了' in text,
            deadline, 'live screen did not appear',
        )
        os.write(master, b'v')
        wait_until(
            capture, proc,
            lambda text: ('タイムフリー' in text and '取得完了' in text and 'オフライン番組' in text
                          and 'ライブ（' not in text),
            deadline, 'time-free screen did not return',
        )
        os.write(master, b'j')
        capture.pump(0.2)
        require(proc.poll() is None, 'TUI exited while moving the selection')
        require('番組一覧' in capture.screen(), 'program list disappeared after selection')
        os.write(master, b'q')
        wait_exit(proc, capture, deadline)
        require(proc.returncode == 0, 'TUI failed on exit')
        require(not output_problems(capture.raw), 'forbidden text in TUI output')
        require(
            termios.tcgetattr(slave) == before and capture.terminal.restored,
            'terminal restoration failed',
        )
        report(phase='smoke_complete', exit_code=proc.returncode, terminal_restored=True,
               rows=rows, columns=columns)
    finally:
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        os.close(master)
        os.close(slave)


def fcntl_winsize(fd, rows, columns):
    import fcntl
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, columns, 0, 0))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/debug/radiko-client'))
    parser.add_argument('--rows', type=int, default=40)
    parser.add_argument('--columns', type=int, default=160)
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error('binary not found')
    if not 20 <= args.rows <= 120 or not 80 <= args.columns <= 300:
        parser.error('rows must be 20..120 and columns 80..300')
    try:
        with tempfile.TemporaryDirectory(prefix='radiko-tui-smoke-') as directory:
            verify(args.binary, directory, args.rows, args.columns)
    except KeyboardInterrupt:
        report(phase='interrupted', completed=False)
        return 130
    except (RuntimeError, ValueError, OSError, subprocess.SubprocessError) as error:
        report(phase='failed', completed=False, error_type=type(error).__name__,
               check=str(error) if type(error) is RuntimeError else None)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
