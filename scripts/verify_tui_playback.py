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
import sys
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

    def frame_matches_size(self):
        return self.grid[-1][0] == '└' and self.grid[-1][-1] == '┘'


def number(text, expression):
    """Read counters from known footer rows, never from titles or schedule content."""
    for line in text.splitlines()[-10:]:
        line = line.strip(' │')
        if expression.startswith('位置'):
            eligible = re.match(r'^\d{2}/\d{2} \d{2}:\d{2}〜\d{2}/\d{2} \d{2}:\d{2} JST / '
                                r'(?:Buffering|Playing|Paused) / 音量\d+% / ', line)
        else:
            eligible = re.match(r'^(?:(?:停止時|直近通知（終了時未集計）):\s*)?今回の出力\s+', line)
        if eligible:
            found = re.search(expression, line)
            if found:
                return float(found.group(1))
    return None


def target_matches(text, station, start, end, mode=None):
    expected = '%s〜%s JST' % (start.strftime('%m/%d %H:%M'), end.astimezone(start.tzinfo).strftime('%m/%d %H:%M'))
    lines = text.splitlines()
    for index, line in enumerate(lines):
        line = line.strip(' │')
        identity = re.match(r'^再生中: \[([A-Za-z0-9_-]+) / (TimeFree|Live)\]', line)
        if identity:
            matches = identity[1] == station and (mode is None or identity[2] == mode)
        else:  # Older fixed binaries put the station/mode after the title.
            identity = re.search(r' / ([A-Za-z0-9_-]+) / (TimeFree|Live)$', line)
            matches = line.startswith('再生中: ') and identity is not None and \
                identity[1] == station and (mode is None or identity[2] == mode)
        if matches and index+1 < len(lines) and expected in lines[index+1]:
            return True
    return False


def failure_diagnostics(text):
    """Only classify a footer notice into fixed labels; never return notice/program text."""
    notice = next((line.strip(' │') for line in text.splitlines()[-8:]
                   if line.strip(' │').startswith('再生停止:')), None)
    if notice is None:
        return dict(notice_visible=False, error_kind=None, http_status=None, source_stage=None)
    categories = [
        ('source_stalled', '30秒間新しいセグメント'),
        ('worker', '再生ワーカーが予期せず終了'),
        ('early_end', '放送枠末尾より前に配信が終了'),
        ('output_device', '音声出力機器の開始・実行に失敗'),
        ('output_stalled', '音声コールバックが進まない'),
        ('source_network', '配信の検証中に通信に失敗'),
        ('source_authentication', '配信認証が拒否'),
        ('source_denied', 'アクセスを拒否'),
        ('source_rate_limited', 'アクセス制限中'),
        ('source_not_found', '対象の番組または配信が見つかりません'),
        ('source_playlist', 'HLSプレイリストまたはサンプルの構造'),
        ('cancelled', '取り消しました'),
        ('aac_header', 'ADTSヘッダーが不正'),
        ('aac_truncated', 'ADTSフレームが途中で切れ'),
        ('aac_format_changed', 'AACの構成が変わりました'),
        ('aac_decode', 'デコード:'),
        ('source_other', '配信取得:'),
    ]
    kind = next((kind for kind, phrase in categories if phrase in notice), 'other')
    status = re.search(r'HTTP (\d{3})', notice)
    stages = [('segment', 'HLS音声取得:'), ('playlist', 'HLS一覧更新:')]
    stage = next((stage for stage, marker in stages if marker in notice), None)
    return dict(notice_visible=True, error_kind=kind,
                http_status=int(status[1]) if status else None, source_stage=stage)


def diagnostic_row(text, prefix):
    for line in text.splitlines()[-10:]:
        line = re.sub(r'^直近通知（終了時未集計）:\s*', '', line.strip(' │'))
        if line.startswith(prefix):
            return re.sub(r'\s+', '', line)
    return ''


def pipeline_diagnostics(text):
    footer = diagnostic_row(text, '処理診断')
    match = re.search(r'処理診断(取得|復号|配送待ち|更新待ち|終了)([0-9]+)ms/'
                      r'取得max([0-9]+)ms/復号([0-9]+)/([0-9]+)ms/音声([0-9]+)ms', footer)
    if match is None:  # Fixed copies of the first diagnostic build use the longer labels.
        match = re.search(r'処理診断(取得|復号|PCM配送待ち|配信更新待ち|処理終了)([0-9]+)ms/'
                          r'取得最大([0-9]+)ms/復号直近([0-9]+)ms・最大([0-9]+)ms/音声([0-9]+)ms', footer)
    if match is None:
        return None
    stages = {'取得': 'acquisition', '復号': 'decode', 'PCM配送待ち': 'backpressure',
              '配信更新待ち': 'waiting', '処理終了': 'finished',
              '配送待ち': 'backpressure', '更新待ち': 'waiting', '終了': 'finished'}
    result = dict(stage=stages[match[1]], elapsed_ms=int(match[2]), acquisition_max_ms=int(match[3]),
                  decode_last_ms=int(match[4]), decode_max_ms=int(match[5]), media_last_ms=int(match[6]))
    http = re.search(r'^通信診断(待機|応答待ち|本文受信|再試行待ち)([0-9]+)ms/'
                     r'応答max([0-9]+)ms/本文max([0-9]+)ms/試行([0-9]+)・再試行待ち([0-9]+)',
                     diagnostic_row(text, '通信診断'))
    if http:
        phases = {'待機': 'idle', '応答待ち': 'headers', '本文受信': 'body', '再試行待ち': 'retry_wait'}
        result['http'] = dict(stage=phases[http[1]], elapsed_ms=int(http[2]), headers_max_ms=int(http[3]),
                              body_max_ms=int(http[4]), attempts=int(http[5]), retry_waits=int(http[6]))
    return result


def wait_exit(proc, terminal, timeout=10):
    deadline = time.monotonic() + timeout
    while proc.poll() is None and time.monotonic() < deadline:
        terminal.pump(0.1)
    require(proc.poll() is not None, 'TUI process exit timed out')
    terminal.pump(0.2)


def device_check(terminal, proc, master, station, selected, ends_at, wait):
    """Observe operator-controlled hardware changes without changing OS routing."""
    def acknowledge(expected):
        report(phase='device_ready', awaiting=expected, muted=True)
        deadline = time.monotonic() + 600
        pending = b''
        last_report = 0
        saw_error = False
        saw_inactive = False
        while time.monotonic() < deadline:
            terminal.pump(0.2)
            text = terminal.screen()
            require(proc.poll() is None, 'TUI exited during device verification')
            saw_error |= '再生停止:' in text
            saw_inactive |= '再生中: なし' in text
            if time.monotonic() - last_report >= 10:
                report(phase='device_observation', awaiting=expected,
                       playback_error_observed=saw_error, inactive_observed=saw_inactive,
                       output_seconds=number(text, r'今回の出力\s+([0-9.]+)秒'))
                last_report = time.monotonic()
            if select.select([sys.stdin], [], [], 0)[0]:
                chunk = os.read(sys.stdin.fileno(), 128)
                require(bool(chunk), 'operator input closed before acknowledgment')
                pending += chunk
                require(len(pending) <= 128, 'operator command exceeds limit')
                if b'\n' in pending:
                    require(pending.strip() == expected.encode(), 'unexpected operator acknowledgment')
                    # This acknowledges an operator action, not proof of the OS route.
                    report(phase='device_acknowledged', action=expected,
                           playback_error_observed=saw_error, inactive_observed=saw_inactive)
                    return
        raise RuntimeError('operator acknowledgment timed out')

    acknowledge('disconnected')
    # Check that application input still responds with the device absent.
    os.write(master, b'?')
    wait(lambda t: '┌操作一覧' in t, 10)
    os.write(master, b'?')
    wait(lambda t: '┌操作一覧' not in t, 10)
    acknowledge('reconnected')
    # A new explicit play request resolves and opens the current default output.
    os.write(master, b's')
    wait(lambda t: '再生中: なし' in t, 10)
    terminal.pump(1)
    os.write(master, b'\r')
    wait(lambda t: 'Playing' in t and target_matches(t, station, selected, ends_at, 'TimeFree') and
         (number(t, r'今回の出力\s+([0-9.]+)秒') or 0) >= 2, 90)
    report(phase='device_replay_started', muted=True)


def sleep_check(terminal, proc, master, station, selected, ends_at, wait):
    """The operator controls sleep; acknowledgment alone does not prove OS sleep."""
    before = wait(lambda t: 'Playing' in t and '音量0%' in t and
                  (number(t, r'今回の出力\s+([0-9.]+)秒') or 0) >= 5)
    report(phase='sleep_ready', muted=True,
           output_seconds=number(before, r'今回の出力\s+([0-9.]+)秒'))
    deadline = time.monotonic() + 600
    pending = b''
    error_observed = False
    inactive_observed = False
    acknowledged = False
    while time.monotonic() < deadline:
        terminal.pump(0.2)
        text = terminal.screen()
        require(proc.poll() is None, 'TUI exited during sleep verification')
        error_observed |= failure_diagnostics(text)['notice_visible']
        inactive_observed |= '再生中: なし' in text
        if select.select([sys.stdin], [], [], 0)[0]:
            chunk = os.read(sys.stdin.fileno(), 128)
            require(bool(chunk), 'operator input closed before wake acknowledgment')
            pending += chunk
            require(len(pending) <= 128, 'operator command exceeds limit')
            if b'\n' in pending:
                require(pending.strip() == b'awake', 'unexpected wake acknowledgment')
                acknowledged = True
                break
    require(acknowledged, 'wake acknowledgment timed out')
    terminal.pump(5)
    text = terminal.screen()
    error_observed |= failure_diagnostics(text)['notice_visible']
    inactive_observed |= '再生中: なし' in text
    result = dict(operator_acknowledged=True, playback_error_observed=error_observed,
                  inactive_observed=inactive_observed, **failure_diagnostics(text),
                  pipeline_last_notification=pipeline_diagnostics(text))
    report(phase='sleep_observed', **result)
    os.write(master, b'?')
    wait(lambda t: '┌操作一覧' in t, 10, check='help unresponsive after wake')
    os.write(master, b'?')
    wait(lambda t: '┌操作一覧' not in t, 10)
    # Explicit stop also handles environments whose output kept running after wake.
    os.write(master, b's')
    wait(lambda t: '再生中: なし' in t, 10)
    terminal.pump(1)
    os.write(master, b'\r')
    wait(lambda t: 'Playing' in t and '音量0%' in t and
         target_matches(t, station, selected, ends_at, 'TimeFree') and
         '停止時:' not in t and not failure_diagnostics(t)['notice_visible'] and
         (number(t, r'今回の出力\s+([0-9.]+)秒') or 0) >= 2, 90,
         check='explicit playback after wake did not start')
    report(phase='sleep_replay_started', muted=True)
    return result


def restart_check(terminal, master, wait, args, selected, ends_at, index, filtered):
    if filtered:
        os.write(master, b'f')
        wait(lambda t: '条件編集中' in t, 10, check='restart filter editor did not open')
        os.write(master, b'\t\t\t\t\t\x15\r')
        wait(lambda t: '条件編集中' not in t and '取得完了' in t, 90,
             check='restart catalog did not finish')
    os.write(master, b'\x1b[H' + b'j' * index)
    terminal.pump(0.3)
    os.write(master, b'\r')
    wait(lambda t: target_matches(t, args.station, selected, ends_at, 'TimeFree')
         and 'Playing' in t and '音量0%' in t and '停止時:' not in t
         and (number(t, r'今回の出力\s+([0-9.]+)秒') or 0) >= 5, 90,
         check='explicit restart did not produce a new output generation')
    os.write(master, b's')
    final = wait(lambda t: '停止しました' in t and '停止時:' in t, 10,
                 check='restart final stop did not appear')
    seconds = number(final, r'今回の出力\s+([0-9.]+)秒')
    underrun = number(final, r'不足\s+(\d+)フレーム')
    require(seconds is not None and underrun is not None, 'restart final counters absent')
    result = dict(output_seconds=seconds, final_underrun_frames=int(underrun), mode='TimeFree')
    report(phase='restart_complete', **result)
    return result


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
    expected_mode = 'Live' if args.live_end else 'TimeFree'
    if args.live_end:
        remaining = (ends_at - datetime.datetime.now(datetime.timezone.utc)).total_seconds()
        require(selected <= datetime.datetime.now(datetime.timezone.utc), 'live slot has not started')
        require(30 < remaining <= args.seconds, 'live end must be 30..seconds away')
    else:
        require(length > args.offset + args.seconds + 10, 'selected slot must exceed offset plus verification duration by ten seconds')
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', Terminal.rows, Terminal.columns, 0, 0))
    before = termios.tcgetattr(slave)
    terminal = Terminal(master)
    proc = None
    completed = False
    last_output, cycles = 0.0, 0
    chapter_scroll_checks = 0
    try:
        proc = subprocess.Popen(command + ['tui', args.station, args.date], stdin=slave,
                                stdout=slave, stderr=slave, start_new_session=True,
                                env=dict(os.environ, TERM='xterm-256color'))

        def wait(predicate, timeout=90, check="expected UI state did not appear"):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                terminal.pump()
                text = terminal.screen()
                require(proc.poll() is None, 'TUI exited early (code=%s)' % proc.returncode)
                if predicate(text):
                    return text
            text = terminal.screen()
            report(phase='ui_wait_timeout', check=check,
                   inactive_visible='再生中: なし' in text,
                   stop_notice_visible='停止しました' in text,
                   stopped_diagnostics_visible='停止時:' in text,
                   playback_error_visible='再生停止:' in text,
                   help_visible='┌操作一覧' in text,
                   editor_visible='条件編集中' in text,
                   chapter_visible='公式AIチャプター・閲覧中' in text,
                   failures_visible='┌取得状況・未取得範囲' in text,
                   **failure_diagnostics(text))
            raise RuntimeError(check)

        wait(lambda t: '取得完了' in t and terminal.frame_matches_size(),
             check='completed TUI frame did not match requested PTY dimensions')
        os.write(master, b'-----')
        wait(lambda t: '音量0%' in t)
        os.write(master, b'j' * index + (b'l' if args.live_end else b'\r'))
        wait(lambda t: '今回の出力' in t and target_matches(t, args.station, selected, ends_at, expected_mode))
        sleep_result = sleep_check(terminal, proc, master, args.station, selected, ends_at, wait) \
            if args.sleep_check else None
        if args.device_check:
            device_check(terminal, proc, master, args.station, selected, ends_at, wait)
        if args.offset:
            os.write(master, b'g' + str(args.offset).encode() + b'\r')
            wait(lambda t: (number(t, r'位置([0-9.]+)秒') or 0) >= args.offset and
                 (number(t, r'今回の出力\s+([0-9.]+)秒') or 0) > 0,
                 check='requested offset did not start output')
        pause_elapsed = None
        if args.pause_seconds:
            wait(lambda t: (number(t, r'今回の出力\s+([0-9.]+)秒') or 0) >= 2)
            os.write(master, b' ')
            paused = wait(lambda t: 'Paused' in t)
            position = number(paused, r'位置([0-9.]+)秒')
            require(position is not None, 'paused position absent')
            paused_at = time.monotonic()
            report(phase='paused', position_seconds=position, requested_seconds=args.pause_seconds,
                   binary_sha256=digest, muted=True)
            next_operation = paused_at
            while time.monotonic() - paused_at < args.pause_seconds:
                terminal.pump(1)
                text = terminal.screen()
                require(proc.poll() is None, 'TUI exited while paused')
                if not ('Paused' in text and target_matches(text, args.station, selected, ends_at, expected_mode)):
                    # Drain a possibly incomplete draw before classifying a state change.
                    terminal.pump(0.5)
                    text = terminal.screen()
                    if not ('Paused' in text and target_matches(text, args.station, selected, ends_at, expected_mode)):
                        report(phase='paused_state_changed', paused_seconds=round(time.monotonic()-paused_at, 1),
                               paused_visible='Paused' in text, playing_visible='Playing' in text,
                               active_target_matches=target_matches(text, args.station, selected, ends_at, expected_mode),
                               inactive_visible='再生中: なし' in text,
                               playback_error_visible='再生停止:' in text,
                               device_error_visible='音声出力機器の開始・実行に失敗' in text,
                               callback_stalled_visible='音声コールバックが進まない' in text,
                               help_visible='┌操作一覧' in text,
                               editor_visible='条件編集中' in text,
                               position_seconds=number(text, r'位置([0-9.]+)秒'))
                        raise RuntimeError('paused playback state or target changed')
                current = number(text, r'位置([0-9.]+)秒')
                require(current is not None and abs(current-position) <= 0.2,
                        'position advanced while paused')
                if time.monotonic() >= next_operation:
                    os.write(master, b'?')
                    wait(lambda t: '┌操作一覧' in t, 10)
                    os.write(master, b'?')
                    wait(lambda t: '┌操作一覧' not in t and 'Paused' in t, 10)
                    os.write(master, b'/pause-cancel')
                    wait(lambda t: '条件編集中' in t, 10)
                    os.write(master, b'\x1b')
                    wait(lambda t: '条件編集中' not in t and 'Paused' in t, 10)
                    # Keep Esc separate from the next key, including parser observation.
                    terminal.pump(0.1)
                    os.write(master, b'r')
                    terminal.pump(0.3)
                    report(phase='paused_progress', paused_seconds=round(time.monotonic()-paused_at, 1),
                           position_seconds=current)
                    next_operation = time.monotonic() + args.interval
            pause_elapsed = time.monotonic() - paused_at
            os.write(master, b' ')
            resumed = wait(lambda t: 'Playing' in t and
                           (number(t, r'位置([0-9.]+)秒') or 0) > position + 1)
            require(target_matches(resumed, args.station, selected, ends_at, expected_mode), 'resumed slot changed')
            report(phase='resumed', paused_seconds=round(pause_elapsed, 1),
                   position_seconds=number(resumed, r'位置([0-9.]+)秒'))
        began = changed = operated = time.monotonic()
        filtered = False
        report(phase='playing', binary_sha256=digest, station=args.station, start=args.start,
               ends_at=ends_at.isoformat(), offset_seconds=args.offset, mode='live_end' if args.live_end else 'timefree', muted=True)
        end_delta = None
        while True:
            terminal.pump(1)
            text = terminal.screen()
            require(proc.poll() is None, 'TUI exited early (code=%s)' % proc.returncode)
            failure = failure_diagnostics(text)
            if failure['notice_visible']:
                report(phase='playback_failure', output_seconds=last_output,
                       underrun_frames=number('\n'.join(text.splitlines()[-8:]), r'不足\s+(\d+)フレーム'),
                       pipeline_last_notification=pipeline_diagnostics(text),
                       **failure)
                raise RuntimeError('playback failed')
            if args.live_end and '番組終了' in text and '再生中: なし' in text:
                end_delta = (datetime.datetime.now(datetime.timezone.utc) - ends_at).total_seconds()
                require(-0.5 <= end_delta <= 5, 'live program end outside wall-clock tolerance')
                break
            require(target_matches(text, args.station, selected, ends_at, expected_mode), 'active playback slot changed')
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
            if cycles % 8 == 0:
                keyword = b'' if filtered else b'__verification_no_match__'
                actions.append(b'f\t\t\t\t\t\x15' + keyword + b'\r')
                filtered = not filtered
            for keys in actions:
                os.write(master, keys)
                terminal.pump(0.3)
                terminal.screen()
            if cycles % 7 == 0:
                os.write(master, b'f\t\t\t\x1bOQ')
                wait(lambda t: '今回観測した候補' in t, 10,
                     check='classification picker did not open')
                os.write(master, b'\x1b')
                wait(lambda t: '今回観測した候補' not in t and '検索条件編集中' in t, 10,
                     check='classification picker did not close')
                os.write(master, b'\x1b')
                wait(lambda t: '検索条件編集中' not in t, 10,
                     check='filter editor did not close')
            if cycles % 9 == 0:
                os.write(master, b'?')
                terminal.pump(0.3)
                require('┌操作一覧' in terminal.screen(), 'help did not open')
                os.write(master, b'?')
                terminal.pump(0.3)
            if cycles % 10 == 0:
                os.write(master, b'e')
                wait(lambda t: '┌取得状況・未取得範囲' in t, 10)
                os.write(master, b'\x1b[Fs')
                terminal.pump(0.3)
                os.write(master, b'q')
                wait(lambda t: '┌取得状況・未取得範囲' not in t and
                     target_matches(t, args.station, selected, ends_at, expected_mode), 10)
            if cycles % 5 == 0:
                if not filtered:
                    os.write(master, b'\x1b[H' + b'j' * index)
                    terminal.pump(0.3)
                os.write(master, b'c')
                terminal.pump(0.3)
                if '公式AIチャプター・閲覧中' in terminal.screen():
                    os.write(master, b'\t')
                    wait(lambda t: '先頭1200文字まで' in t, 10,
                         check='chapter summary focus did not appear')
                    os.write(master, b'\x1b[F')
                    terminal.pump(0.2)
                    os.write(master, b'\t')
                    wait(lambda t: 'Tabで拡大' in t, 10,
                         check='chapter list focus did not return')
                    chapter_scroll_checks += 1
                    os.write(master, b'\x1b')
                    wait(lambda t: '公式AIチャプター・閲覧中' not in t and
                         target_matches(t, args.station, selected, ends_at, expected_mode), 10,
                         check='chapter view did not close')
            text = terminal.screen()
            require('音量0%' in text and not any(marker in text for marker in [
                '┌検索条件編集中', '┌条件編集中', '┌ログイン（パスワード非表示）',
                '┌操作一覧', '公式AIチャプター・閲覧中', '┌取得状況・未取得範囲',
            ]), 'operation did not return to muted normal mode')
            report(phase='progress', output_seconds=last_output, operation_cycles=cycles,
                   wall_seconds=round(time.monotonic()-began),
                   underrun_frames=number(text, r'不足\s+(\d+)フレーム'),
                   pipeline=pipeline_diagnostics(text))
            operated = time.monotonic()
        if args.live_end:
            final = text
            require('停止時:' in final, 'final live diagnostics absent')
            terminal.pump(1)
            require('再生中: なし' in terminal.screen(), 'playback auto-advanced after program end')
        else:
            os.write(master, b's')
            final = wait(lambda t: '停止しました' in t and '停止時:' in t, 10,
                         check='final stop confirmation did not appear')
        underrun = number(final, r'不足\s+(\d+)フレーム')
        require(underrun is not None, 'final underrun counter absent')
        final_output = number(final, r'今回の出力\s+([0-9.]+)秒')
        require(final_output is not None and final_output >= last_output, 'final output counter absent or reset')
        last_output = final_output
        report(phase='stopped', output_seconds=last_output,
               final_underrun_frames=int(underrun), live_end_delta_seconds=end_delta)
        restarted = restart_check(terminal, master, wait, args, selected, ends_at, index, filtered) \
            if args.restart_check else None
        os.write(master, b'q')
        wait_exit(proc, terminal)
        require(proc.returncode == 0, 'TUI failed on exit')
        require(termios.tcgetattr(slave) == before and terminal.restored, 'terminal restoration failed')
        report(phase='complete', output_seconds=last_output, operation_cycles=cycles,
               final_underrun_frames=int(underrun), confirmed_searches=cycles//8,
               chapter_scroll_checks=chapter_scroll_checks,
               terminal_restored=True, exit_code=proc.returncode, binary_sha256=digest,
               live_end_delta_seconds=end_delta, paused_seconds=pause_elapsed, restarted=restarted, sleep_check=sleep_result)
        completed = True
    finally:
        try:
            if proc is not None:
                if proc.poll() is None:
                    try:
                        os.write(master, b'\x03')
                    except OSError:  # The child can exit between poll() and write().
                        pass
                    terminal.pump(0.5)
                    if proc.poll() is None:
                        proc.terminate()
                try:
                    wait_exit(proc, terminal)
                except RuntimeError:
                    proc.kill()
                    proc.wait(timeout=5)
                if not completed:
                    try:
                        attributes_restored = termios.tcgetattr(slave) == before
                    except OSError:
                        attributes_restored = False
                    report(phase='cleanup', child_exit_code=proc.returncode,
                           terminal_attributes_restored=attributes_restored,
                           alternate_screen_restored=terminal.restored,
                           terminal_restored=attributes_restored and terminal.restored)
        finally:
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
    parser.add_argument('--offset', type=int, default=0, help='initial time-free position in integer seconds')
    parser.add_argument('--interval', type=int, default=45)
    parser.add_argument('--device-check', action='store_true',
                        help='operator-guided output disconnect/reconnect; acknowledge disconnected and reconnected on stdin')
    parser.add_argument('--sleep-check', action='store_true',
                        help='operator-controlled sleep/wake; acknowledge awake on stdin, then explicitly replay')
    parser.add_argument('--restart-check', action='store_true',
                        help='after the measured stop, replay the selected time-free slot for five seconds and stop again')
    parser.add_argument('--pause-seconds', type=int, default=0,
                        help='pause time-free playback for 1..86400 seconds, check position and resume before output verification')
    parser.add_argument('--live-end', action='store_true', help='play selected on-air slot until its natural end')
    args = parser.parse_args()
    if not 1 <= args.seconds <= 86400 or not 1 <= args.interval <= 300:
        parser.error('seconds must be 1..86400 and interval 1..300')
    if not 0 <= args.pause_seconds <= 86400 or (args.pause_seconds and args.live_end):
        parser.error('pause-seconds must be 0..86400 and cannot be combined with live-end')
    if not 0 <= args.offset <= 86400 or (args.offset and args.live_end):
        parser.error('offset must be 0..86400 and is only available for time-free')
    if args.device_check and (args.live_end or args.pause_seconds):
        parser.error('device-check cannot be combined with live-end or pause-seconds')
    if args.sleep_check and (args.live_end or args.device_check or args.pause_seconds):
        parser.error('sleep-check cannot be combined with live-end, device-check or pause-seconds')
    if args.restart_check and args.live_end:
        parser.error('restart-check is only available for time-free')
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
