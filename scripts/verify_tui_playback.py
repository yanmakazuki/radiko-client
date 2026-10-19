#!/usr/bin/env python3
"""Explicit macOS network/audio verification; always muted, never an ordinary test.

Only target identifiers and diagnostics are printed. Schedule content, terminal state, and audio
stay in memory. A private binary copy survives rebuilding target/debug during a run.
"""
import argparse
import codecs
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
import unicodedata


def report(**values):
    print(json.dumps(values, ensure_ascii=False), flush=True)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


class Terminal:
    """Bounded, incremental projection of the CSI operations emitted by Ratatui."""

    rows, columns = 40, 160
    escapes = re.compile(r'\x1b\[([0-?]*)([ -/]*)([@-~])')

    def __init__(self, fd):
        self.fd = fd
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.pending = ''
        self.grid = [[' '] * self.columns for _ in range(self.rows)]
        self.row = self.col = 0
        self.restored = False

    def pump(self, seconds=0.2):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([self.fd], [], [], min(0.05, max(0, until-time.monotonic())))[0]:
                try:
                    chunk = os.read(self.fd, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                self.feed(chunk)

    def feed(self, chunk):
        self.pending += self.decoder.decode(chunk)
        require(len(self.pending) <= 131072, 'terminal control sequence limit exceeded')
        end = 0
        while end < len(self.pending):
            char = self.pending[end]
            if char == '\x1b':
                if end+1 >= len(self.pending):
                    break
                kind = self.pending[end+1]
                if kind == '[':
                    match = self.escapes.match(self.pending, end)
                    if match is None:
                        break
                    self.control(match.group(1), match.group(3))
                    end = match.end()
                    continue
                if kind == ']':
                    stop = re.search(r'\x07|\x1b\\', self.pending[end+2:])
                    if stop is None:
                        break
                    end += 2 + stop.end()
                    continue
                size = 3 if kind in '()' else 2
                if end+size > len(self.pending):
                    break
                end += size
                continue
            if char == '\r':
                self.col = 0
            elif char == '\n':
                self.row = min(self.rows-1, self.row+1)
            elif ord(char) >= 32 and not unicodedata.combining(char):
                width = 2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1
                if self.col >= self.columns:
                    self.col = 0
                    self.row = min(self.rows-1, self.row+1)
                self.grid[self.row][self.col] = char
                if width == 2 and self.col+1 < self.columns:
                    self.grid[self.row][self.col+1] = ''
                self.col += width
            end += 1
        self.pending = self.pending[end:]

    def control(self, params, code):
        if params.startswith('?'):
            self.restored |= params == '?1049' and code == 'l'
            return
        values = [int(v or 0) for v in params.split(';')]
        n = values[0] or 1
        if code in ('H', 'f'):
            self.row = min(self.rows-1, max(0, n-1))
            self.col = min(self.columns-1, max(0, (values[1] if len(values)>1 else 1)-1))
        elif code == 'G': self.col = min(self.columns-1, max(0, n-1))
        elif code == 'd': self.row = min(self.rows-1, max(0, n-1))
        elif code == 'A': self.row = max(0, self.row-n)
        elif code == 'B': self.row = min(self.rows-1, self.row+n)
        elif code == 'C': self.col = min(self.columns-1, self.col+n)
        elif code == 'D': self.col = max(0, self.col-n)
        elif code == 'J' and values[0] in (2, 3):
            self.grid = [[' '] * self.columns for _ in range(self.rows)]
        elif code == 'K':
            if values[0] == 0: self.grid[self.row][self.col:] = [' '] * (self.columns-self.col)
            elif values[0] == 2: self.grid[self.row] = [' '] * self.columns

    def screen(self):
        return '\n'.join(''.join(line) for line in self.grid)


def number(text, expression):
    found = re.search(expression, text)
    return float(found.group(1)) if found else None


def target_matches(text, station, start, end):
    expected = '%s〜%s JST' % (start.strftime('%m/%d %H:%M'), end.astimezone(start.tzinfo).strftime('%m/%d %H:%M'))
    lines = text.splitlines()
    return any('再生中:' in line and '/ %s /' % station in line
               and index+1 < len(lines) and expected in lines[index+1]
               for index, line in enumerate(lines))


def wait_exit(proc, terminal, timeout=10):
    deadline = time.monotonic() + timeout
    while proc.poll() is None and time.monotonic() < deadline:
        terminal.pump(0.1)
    require(proc.poll() is not None, 'TUI process exit timed out')
    terminal.pump(0.2)


def verify(args, directory):
    os.chmod(directory, 0o700)
    binary = Path(directory) / 'radiko-client'
    shutil.copyfile(args.binary, binary)
    binary.chmod(0o700)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    session = Path(directory) / 'session.json'
    # Exclusive creation with restrictive permissions, including the copy interval.
    with args.session_file.open('rb') as source, os.fdopen(os.open(session, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as dest:
        saved = source.read(16 * 1024 + 1)
        require(len(saved) <= 16 * 1024, 'session exceeds application size limit')
        dest.write(saved)
    command = [str(binary), '--data-dir', directory]
    if args.date == 'auto':
        jst = datetime.timezone(datetime.timedelta(hours=9))
        args.date = (datetime.datetime.now(jst) - datetime.timedelta(hours=5)).strftime('%Y%m%d')
    listing = subprocess.run(command + ['programs', args.station, args.date, '--json'],
                             capture_output=True, timeout=90)
    require(listing.returncode == 0, 'public program acquisition failed')
    programs = sorted(json.loads(listing.stdout)['programs'], key=lambda p: p['starts_at'])
    if args.start == 'auto':
        require(args.live_end, 'automatic slot selection is only supported for live end verification')
        now = datetime.datetime.now(datetime.timezone.utc)
        on_air = [p for p in programs if datetime.datetime.fromisoformat(p['starts_at']) <= now
                  and 30 < (datetime.datetime.fromisoformat(p['ends_at'])-now).total_seconds() <= args.seconds]
        require(bool(on_air), 'no on-air slot ends within the requested verification window')
        args.start = datetime.datetime.fromisoformat(on_air[0]['starts_at']).strftime('%Y%m%d%H%M%S')
    selected = datetime.datetime.strptime(args.start, '%Y%m%d%H%M%S').replace(
        tzinfo=datetime.timezone(datetime.timedelta(hours=9)))
    index = next((i for i, p in enumerate(programs)
                  if datetime.datetime.fromisoformat(p['starts_at']) == selected), None)
    require(index is not None, 'selected slot absent from acquired schedule')
    length = (datetime.datetime.fromisoformat(programs[index]['ends_at']) - selected).total_seconds()
    ends_at = datetime.datetime.fromisoformat(programs[index]['ends_at'])
    if args.live_end:
        remaining = (ends_at - datetime.datetime.now(datetime.timezone.utc)).total_seconds()
        require(selected <= datetime.datetime.now(datetime.timezone.utc), 'live slot has not started')
        require(30 < remaining <= args.seconds, 'live end must be 30..seconds away')
    else:
        require(length > args.seconds + 10, 'selected slot must exceed verification duration by ten seconds')
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', Terminal.rows, Terminal.columns, 0, 0))
    before = termios.tcgetattr(slave)
    terminal = Terminal(master)
    proc = None
    last_output, cycles = 0.0, 0
    try:
        proc = subprocess.Popen(command + ['tui', args.station, args.date], stdin=slave,
                                stdout=slave, stderr=slave, env=dict(os.environ, TERM='xterm-256color'))

        def wait(predicate, timeout=90):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                terminal.pump()
                text = terminal.screen()
                require(proc.poll() is None, 'TUI exited early (code=%s)' % proc.returncode)
                if predicate(text):
                    return text
            raise RuntimeError('expected UI state did not appear')

        wait(lambda t: '取得完了' in t)
        os.write(master, b'-----')
        wait(lambda t: '音量0%' in t)
        os.write(master, b'j' * index + (b'l' if args.live_end else b'\r'))
        wait(lambda t: '今回の出力' in t and target_matches(t, args.station, selected, ends_at))
        began = changed = operated = time.monotonic()
        filtered = False
        report(phase='playing', binary_sha256=digest, station=args.station, start=args.start,
               ends_at=ends_at.isoformat(), mode='live_end' if args.live_end else 'timefree', muted=True)
        end_delta = None
        while True:
            terminal.pump(1)
            text = terminal.screen()
            require(proc.poll() is None, 'TUI exited early (code=%s)' % proc.returncode)
            require('再生停止:' not in text, 'playback failed')
            if args.live_end and '番組終了' in text and '再生中: なし' in text:
                end_delta = (datetime.datetime.now(datetime.timezone.utc) - ends_at).total_seconds()
                require(-0.5 <= end_delta <= 5, 'live program end outside wall-clock tolerance')
                break
            require(target_matches(text, args.station, selected, ends_at), 'active playback slot changed')
            output = number(text, r'今回の出力\s+([0-9.]+)秒')
            if output is not None:
                require(output >= last_output, 'unexpected output generation reset')
                if output > last_output:
                    changed = time.monotonic()
                last_output = output
            require(time.monotonic()-changed < 60, 'output stalled for sixty seconds')
            require(time.monotonic()-began < args.seconds+180, 'verification wall-clock limit exceeded')
            if not args.live_end and last_output >= args.seconds:
                break
            if time.monotonic()-operated < args.interval:
                continue
            cycles += 1
            actions = [b'/verification-cancel\x1b']
            if cycles % 2 == 0: actions.append(b'f\x1b')
            if cycles % 3 == 0: actions.append(b'r')
            if cycles % 4 == 0: actions.extend([b'v', b'v'])
            if cycles % 6 == 0: actions.extend([b'I', b'\x1b'])
            if cycles % 7 == 0: actions.extend([b'f\t\t\t\x1bOQ', b'\x1b\x1b'])
            if cycles % 8 == 0:
                keyword = b'' if filtered else b'__verification_no_match__'
                actions.append(b'f\t\t\t\t\t\x15' + keyword + b'\r')
                filtered = not filtered
            for keys in actions:
                os.write(master, keys)
                terminal.pump(0.3)
                terminal.screen()
            if cycles % 9 == 0:
                os.write(master, b'?')
                terminal.pump(0.3)
                require('操作一覧' in terminal.screen(), 'help did not open')
                os.write(master, b'?')
                terminal.pump(0.3)
            if cycles % 5 == 0:
                os.write(master, b'c')
                terminal.pump(0.3)
                if '公式AIチャプター・閲覧中' in terminal.screen():
                    os.write(master, b'\x1b')
                    terminal.pump(0.3)
            text = terminal.screen()
            require('音量0%' in text, 'operation did not return to muted normal mode')
            report(phase='progress', output_seconds=last_output, operation_cycles=cycles,
                   wall_seconds=round(time.monotonic()-began),
                   underrun_frames=number(text, r'不足\s+(\d+)フレーム'))
            operated = time.monotonic()
        if args.live_end:
            final = text
            require('停止時:' in final, 'final live diagnostics absent')
            terminal.pump(1)
            require('再生中: なし' in terminal.screen(), 'playback auto-advanced after program end')
        else:
            os.write(master, b's')
            final = wait(lambda t: '停止しました' in t and '停止時:' in t, 10)
        underrun = number(final, r'不足\s+(\d+)フレーム')
        require(underrun is not None, 'final underrun counter absent')
        final_output = number(final, r'今回の出力\s+([0-9.]+)秒')
        require(final_output is not None and final_output >= last_output, 'final output counter absent or reset')
        last_output = final_output
        report(phase='stopped', output_seconds=last_output,
               final_underrun_frames=int(underrun), live_end_delta_seconds=end_delta)
        os.write(master, b'q')
        wait_exit(proc, terminal)
        require(proc.returncode == 0, 'TUI failed on exit')
        require(termios.tcgetattr(slave) == before and terminal.restored, 'terminal restoration failed')
        report(phase='complete', output_seconds=last_output, operation_cycles=cycles,
               final_underrun_frames=int(underrun), confirmed_searches=cycles//8,
               terminal_restored=True, exit_code=proc.returncode, binary_sha256=digest,
               live_end_delta_seconds=end_delta)
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
    parser.add_argument('--session-file', type=Path, required=True)
    parser.add_argument('--station', default='TBS')
    parser.add_argument('--date', required=True, help='broadcast day YYYYMMDD, or auto for the current JST broadcast day')
    parser.add_argument('--start', required=True, help='slot start YYYYMMDDHHmmss (JST), or auto with --live-end')
    parser.add_argument('--seconds', type=int, default=1800)
    parser.add_argument('--interval', type=int, default=45)
    parser.add_argument('--live-end', action='store_true', help='play selected on-air slot until its natural end')
    args = parser.parse_args()
    if not 1 <= args.seconds <= 86400 or not 1 <= args.interval <= 300:
        parser.error('seconds must be 1..86400 and interval 1..300')
    def interrupt(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupt)
    try:
        with tempfile.TemporaryDirectory(prefix='radiko-tui-verification-') as directory:
            verify(args, directory)
    except KeyboardInterrupt:
        report(phase='interrupted', completed=False)
        return 130
    except (RuntimeError, ValueError, OSError, subprocess.SubprocessError) as error:
        # Only our fixed RuntimeError checks are safe to print; hide external exceptions.
        report(phase='failed', error_type=type(error).__name__, completed=False,
               check=str(error) if type(error) is RuntimeError else None)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
