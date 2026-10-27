#!/usr/bin/env python3
"""Explicit nationwide TUI load check. No account, audio, or metadata persistence.

peak_child_rss_bytes is bytes. Linux ru_maxrss is KiB and is multiplied by 1024.
macOS ru_maxrss is already bytes and is reported unchanged.
"""
import argparse
import datetime
import fcntl
import hashlib
import os
from pathlib import Path
import pty
import re
import resource
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time

from verify_tui_playback import Terminal, report, require, wait_exit


def child_maxrss_bytes(ru_maxrss, platform):
    """Return ru_maxrss in bytes. Linux reports KiB; darwin reports bytes."""
    if platform == 'linux':
        return ru_maxrss * 1024
    return ru_maxrss


def verify(args, directory):
    os.chmod(directory, 0o700)
    Terminal.rows, Terminal.columns = args.rows, args.columns
    binary = Path(directory) / 'radiko-client'
    shutil.copyfile(args.binary, binary)
    binary.chmod(0o700)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', Terminal.rows, Terminal.columns, 0, 0))
    before = termios.tcgetattr(slave)
    terminal = Terminal(master)
    proc = None
    began = operated = time.monotonic()
    latencies = []
    try:
        proc = subprocess.Popen([str(binary), '--data-dir', directory, 'tui', args.station or '--all', args.start, args.end],
                                stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
                                env=dict(os.environ, TERM='xterm-256color'))
        report(phase='catalog_started', start=args.start, end=args.end, binary_sha256=digest)

        def wait(predicate, timeout=15):
            until = time.monotonic() + timeout
            while time.monotonic() < until:
                terminal.pump(0.05)
                require(proc.poll() is None, 'TUI exited before verification completed')
                text = terminal.screen()
                if predicate(text):
                    return text
            raise RuntimeError('catalog UI response timed out')

        def check_keys():
            sent = time.monotonic()
            os.write(master, b'?')
            wait(lambda t: '┌操作一覧' in t and terminal.frame_matches_size())
            latencies.append(time.monotonic()-sent)
            heading = re.search(r'操作一覧 (\d+)/(\d+)行', terminal.screen())
            require(heading is not None, 'help scroll heading absent')
            expected = max(1, int(heading[2]) - max(0, Terminal.rows-7) + 1)
            os.write(master, b'\x1b[F')
            wait(lambda t: '操作一覧 %s/' % expected in t)
            os.write(master, b'\x1b[H')
            wait(lambda t: '操作一覧 1/' in t)
            os.write(master, b'?')
            wait(lambda t: '┌操作一覧' not in t)
            # Exercise global first/last selection and page movement; never press Enter.
            for keys in [b'\x1b[F', b'\x1b[5~', b'\x1b[H', b'\x1b[6~']:
                os.write(master, keys)
                terminal.pump(0.1)

        wait(lambda t: '番組一覧' in t)
        while True:
            terminal.pump(0.2)
            text = terminal.screen()
            require(proc.poll() is None, 'TUI exited during acquisition')
            require(not re.search(r'^[│ ]*取得失敗 /', text, re.MULTILINE), 'catalog acquisition failed')
            require(time.monotonic()-began < args.timeout, 'catalog wall-clock limit exceeded')
            if time.monotonic()-operated >= 10:
                check_keys()
                text = terminal.screen()
                counts = re.search(r'一致(\d+) / 取得(\d+)枠', text)
                report(phase='catalog_progress', wall_seconds=round(time.monotonic()-began, 1),
                       source_programs=int(counts[2]) if counts else None,
                       help_latency_ms=round(latencies[-1]*1000, 1))
                operated = time.monotonic()
            if re.search(r'^[│ ]*(取得完了|部分取得・未取得あり) /', text, re.MULTILINE):
                break
        completed_after = time.monotonic()-began
        partial = bool(re.search(r'^[│ ]*部分取得・未取得あり /', text, re.MULTILINE))
        counts = re.search(r'一致(\d+) / 取得(\d+)枠', text)
        require(counts is not None, 'catalog counts absent')
        # Measure interaction with the completed full result as well as acquisition.
        for _ in range(3):
            check_keys()
        os.write(master, b'e')
        failure_text = wait(lambda t: '┌取得状況・未取得範囲' in t)
        missing = re.search(r'地域失敗(\d+) / 局・日失敗(\d+) / 不正枠(\d+)', failure_text)
        require(missing is not None, 'failure summary absent')
        examples = []
        for line in failure_text.splitlines():
            match = re.search(r'(地域|番組表|不正枠除外) ([A-Za-z0-9_-]+) (\d{4}-\d{2}-\d{2})? /', line)
            if match:
                examples.append(dict(kind=match[1], target=match[2], day=match[3],
                                     not_found='見つかりません' in line))
        os.write(master, b'e')
        wait(lambda t: '┌取得状況・未取得範囲' not in t)
        os.write(master, b'q')
        wait_exit(proc, terminal)
        require(proc.returncode == 0, 'catalog TUI failed on exit')
        require(termios.tcgetattr(slave) == before and terminal.restored, 'terminal restoration failed')
        report(phase='catalog_complete', complete=not partial,
               source_programs=int(counts[2]), matched_programs=int(counts[1]),
               acquisition_seconds=round(completed_after, 2),
               visible_failure_examples=examples[:5],
               area_failures=int(missing[1]), schedule_failures=int(missing[2]), invalid_programs=int(missing[3]),
               peak_child_rss_bytes=child_maxrss_bytes(
                   resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss, sys.platform),
               help_checks=len(latencies), help_median_ms=round(statistics.median(latencies)*1000, 1),
               help_max_ms=round(max(latencies)*1000, 1), terminal_restored=True,
               exit_code=proc.returncode, binary_sha256=digest, rows=args.rows, columns=args.columns)
    finally:
        if proc is not None and proc.poll() is None:
            os.write(master, b'\x03')
            terminal.pump(0.5)
            if proc.poll() is None:
                proc.terminate()
            try:
                wait_exit(proc, terminal)
            except RuntimeError:
                proc.kill()
                proc.wait()
        os.close(master)
        os.close(slave)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/debug/radiko-client'))
    parser.add_argument('--rows', type=int, default=40)
    parser.add_argument('--columns', type=int, default=160)
    parser.add_argument('--station', help='single station instead of nationwide acquisition')
    parser.add_argument('--start', required=True)
    parser.add_argument('--end', required=True)
    parser.add_argument('--timeout', type=int, default=1800)
    args = parser.parse_args()
    try:
        start, end = (datetime.datetime.strptime(value, '%Y%m%d').date() for value in [args.start, args.end])
    except ValueError:
        parser.error('dates must be YYYYMMDD')
    if not 0 <= (end-start).days < 30 or not 1 <= args.timeout <= 7200:
        parser.error('range must be 1..30 days and timeout 1..7200 seconds')

    if not 20 <= args.rows <= 120 or not 80 <= args.columns <= 300:
        parser.error('rows must be 20..120 and columns 80..300')

    def interrupt(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupt)
    try:
        with tempfile.TemporaryDirectory(prefix='radiko-catalog-verification-') as directory:
            verify(args, directory)
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
