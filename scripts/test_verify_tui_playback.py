"""Offline tests for measurement parsing; no radiko account, network, or audio."""
import unittest
from unittest import mock
import os
import pty
import subprocess
import sys
import datetime
import fcntl
import struct
import termios
import json
import tempfile
import signal
from pathlib import Path

from verify_tui_playback import Terminal, wait_exit, target_matches, failure_diagnostics, pipeline_diagnostics, sleep_check, device_check, number, select_program, failure_observation, check_timefree_end, UnderrunObservations, output_has_started, require_playback_alive, require_no_alsa_noise


class TerminalTests(unittest.TestCase):
    def test_playing_evidence_requires_positive_output_from_the_requested_slot(self):
        start = datetime.datetime.fromisoformat('2026-10-06T12:00:00+09:00')
        end = start + datetime.timedelta(hours=1)
        initial = ('再生中: [TBS / TimeFree] title 今回の出力 999秒\n'
                   '10/06 12:00〜10/06 13:00 JST / Buffering / 音量0% / 位置0.0秒\n'
                   '今回の出力 0.0秒 / 不足 0フレーム')
        self.assertFalse(output_has_started(initial, 'TBS', start, end, 'TimeFree'))
        positive = initial.replace('今回の出力 0.0秒', '今回の出力 0.1秒')
        self.assertTrue(output_has_started(positive, 'TBS', start, end, 'TimeFree'))
        self.assertFalse(output_has_started(positive, 'LFR', start, end, 'TimeFree'))
        self.assertFalse(output_has_started(positive, 'TBS', start, end, 'Live'))
        with mock.patch('verify_tui_playback.report') as report:
            require_playback_alive(initial, 0)
            report.assert_not_called()
            stopped = initial + '\n再生停止: 音声コールバックが進まない synthetic-secret'
            with self.assertRaisesRegex(RuntimeError, 'playback failed'):
                require_playback_alive(stopped, 0)
            self.assertEqual(report.call_args.kwargs['error_kind'], 'output_stalled')
            self.assertEqual(report.call_args.kwargs['output_seconds'], 0)
            self.assertNotIn('synthetic-secret', str(report.call_args))

    def test_underrun_observations_are_numeric_capped_and_keep_the_last_increase(self):
        observer = UnderrunObservations()
        def screen(frames):
            return ('title sensitive-url 不足 999999フレーム\n'
                    f'今回の出力 12.3秒 / 不足 {frames}フレーム\n'
                    '処理診断 取得1000ms / 取得max2000ms / 復号500/600ms / 音声5000ms')
        self.assertIsNone(observer.observe(screen(0)))
        self.assertIsNone(observer.observe('sensitive-url 不足 999999フレーム'))
        for index in range(1, 12):
            result = observer.observe(screen(index*10), at_stop=index == 11)
            self.assertEqual(result is not None, index <= 8)
            if result:
                self.assertEqual(result['delta_frames'], 10)
                self.assertEqual(result['pipeline']['stage'], 'acquisition')
        self.assertIsNone(observer.observe(screen(110)))
        summary = observer.summary()
        self.assertEqual(summary['increases_observed'], 11)
        self.assertEqual(summary['samples_reported'], 8)
        self.assertEqual(summary['last']['underrun_frames'], 110)
        self.assertEqual(summary['last']['sample'], 'stopped')
        self.assertNotIn('sensitive-url', str(summary))
        with mock.patch('verify_tui_playback.report') as report:
            with self.assertRaisesRegex(RuntimeError, 'playback failed'):
                require_playback_alive(screen(110) + '\n再生停止: 音声コールバックが進まない',
                                       12.3, observer)
            self.assertEqual(report.call_args.kwargs['underrun_observations'], summary)
        with self.assertRaisesRegex(RuntimeError, 'counter reset'):
            observer.observe(screen(0))

    def test_timefree_natural_end_checks_remaining_media_and_final_generation(self):
        programs = [dict(starts_at='2026-10-06T12:00:00+09:00', ends_at='2026-10-06T13:00:00+09:00')]
        now = datetime.datetime.fromisoformat('2026-10-23T09:00:00+09:00')
        selected, index, end = select_program(programs, '20261006120000', 15, 3590, False, False, now, True)
        self.assertEqual(index, 0)
        self.assertEqual((end-selected).total_seconds(), 3600)
        for seconds, offset in [(9, 3590), (15, 3596), (15, 3600), (15, 0)]:
            with self.subTest(seconds=seconds, offset=offset), self.assertRaises(RuntimeError):
                select_program(programs, '20261006120000', seconds, offset, False, False, now, True)
        with self.assertRaises(RuntimeError):
            select_program(programs, '20261006120000', 15, 3590, False, False, selected, True)
        final = ('synthetic title 今回の出力 999秒\n再生中: なし\n'
                 '停止時: 今回の出力 10.0秒 / 不足 0フレーム\n番組終了')
        self.assertEqual(check_timefree_end(final, 10), 10)
        for invalid in [final.replace('10.0秒', '5.0秒'), final.replace('番組終了', '停止しました'),
                        final.replace('番組終了', 'synthetic title 番組終了'),
                        final.replace('再生中: なし', '再生中: [TBS / TimeFree]'),
                        final.replace('停止時:', '直近通知（終了時未集計）:')]:
            with self.subTest(text=invalid), self.assertRaises(RuntimeError):
                check_timefree_end(invalid, 10)

    def test_timefree_end_incompatible_checks_fail_before_session_io(self):
        script = Path(__file__).with_name('verify_tui_playback.py')
        with tempfile.TemporaryDirectory() as directory:
            base = [sys.executable, str(script), '--session-file', str(Path(directory) / 'absent.json'),
                    '--date', '20261006', '--start', '20261006120000', '--timefree-end']
            for options in [['--live'], ['--live-end'], ['--pause-seconds', '1'],
                            ['--device-check'], ['--sleep-check'], ['--restart-check']]:
                with self.subTest(options=options):
                    result = subprocess.run(base+options, capture_output=True, timeout=5,
                                            env=dict(os.environ, TMPDIR=directory))
                    self.assertEqual(result.returncode, 2)
                    self.assertEqual(result.stdout, b'')
                    self.assertEqual(list(Path(directory).iterdir()), [])

    def _assert_termination_removes_temporary_directory(self, termination, preexec_fn=None):
        # Run main's real signal/TemporaryDirectory boundary with acquisition replaced.
        # This does not start radiko-client or read a session.
        code = """
import json, sys, time
import verify_tui_playback as verification
def fake_verify(args, directory):
    print(json.dumps(dict(ready=directory)), flush=True)
    while True:
        time.sleep(0.01)
verification.verify = fake_verify
sys.argv = ['verify', '--session-file', 'unused', '--date', '20261021', '--start', '20261021120000']
raise SystemExit(verification.main())
"""
        child = subprocess.Popen([sys.executable, '-c', code],
                                 cwd=Path(__file__).parent,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 text=True, preexec_fn=preexec_fn)
        try:
            directory = Path(json.loads(child.stdout.readline())['ready'])
            self.assertTrue(directory.is_dir())
            child.send_signal(termination)
            output, errors = child.communicate(timeout=5)
            self.assertEqual(child.returncode, 130, errors)
            self.assertEqual(json.loads(output)['phase'], 'interrupted')
            self.assertFalse(directory.exists())
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=5)

    def test_termination_signals_remove_the_verifier_temporary_directory(self):
        for termination in [signal.SIGTERM, signal.SIGHUP, signal.SIGINT]:
            with self.subTest(signal=termination):
                self._assert_termination_removes_temporary_directory(termination)

    def test_sigint_removes_the_temporary_directory_when_the_parent_ignored_it(self):
        # An inherited SIG_IGN suppresses Python's default KeyboardInterrupt handler.
        def ignore_sigint():
            signal.signal(signal.SIGINT, signal.SIG_IGN)

        self._assert_termination_removes_temporary_directory(signal.SIGINT, ignore_sigint)

    def test_failure_observation_reads_the_ninth_footer_row_without_copying_notice_text(self):
        text = '\n'.join([
            'description synthetic-secret 不足 999999フレーム',
            '直近通知（終了時未集計）: 今回の出力 12.3秒 / 不足 123フレーム',
            '直近通知（終了時未集計）: 処理診断 終了0ms / 取得max42ms / 復号25/80ms / 音声5000ms',
            '通信診断 待機0ms / 応答max42ms / 本文max3ms / 試行5・再試行待ち1 / 通信失敗1・期限1・接続0',
            '操作案内1', '操作案内2', '操作案内3', '操作案内4',
            '再生停止: 配信取得: HLS音声取得: 対象の番組または配信が見つかりません synthetic-secret',
            '└─────┘',
        ])
        observed = failure_observation(text, 10.0)
        self.assertEqual(observed['output_seconds'], 12.3)
        self.assertEqual(observed['previous_output_seconds'], 10.0)
        self.assertEqual(observed['underrun_frames'], 123)
        self.assertEqual(observed['source_stage'], 'segment')
        self.assertEqual(observed['pipeline_last_notification']['http']['timeouts'], 1)
        self.assertNotIn('synthetic-secret', str(observed))
        self.assertIsNone(failure_observation('再生中: [TBS / TimeFree] 再生停止: synthetic-secret', 0))

    def test_live_slot_selection_separates_bounded_output_from_natural_end(self):
        programs = [dict(starts_at='2026-10-21T23:00:00+09:00', ends_at='2026-10-22T01:00:00+09:00')]
        now = datetime.datetime.fromisoformat('2026-10-22T00:50:00+09:00')
        selected, index, end = select_program(programs, 'auto', 60, 0, True, False, now)
        self.assertEqual(selected.strftime('%Y%m%d%H%M%S'), '20261021230000')
        self.assertEqual(index, 0)
        self.assertEqual((end-now).total_seconds(), 600)
        with self.assertRaises(RuntimeError):
            select_program(programs, 'auto', 60, 0, False, True, now)
        natural = select_program(programs, 'auto', 600, 0, False, True, now)
        self.assertEqual(natural, (selected, index, end))
        for start in ['auto', '20261021230000']:
            with self.subTest(start=start):
                with self.assertRaises(RuntimeError):
                    select_program(programs, start, 570, 0, True, False, now)
                with self.assertRaises(RuntimeError):
                    select_program(programs, start, 60, 0, True, False, end)
                with self.assertRaises(RuntimeError):
                    select_program(programs, start, 60, 0, True, False,
                                   selected-datetime.timedelta(seconds=1))
        with self.assertRaises(RuntimeError):
            select_program(programs, 'auto', 60, 0, False, False, now)

    def test_incompatible_live_arguments_fail_before_reading_the_session_or_starting_io(self):
        script = Path(__file__).with_name('verify_tui_playback.py')
        with tempfile.TemporaryDirectory() as directory:
            session = Path(directory) / 'must-not-be-read.json'
            base = [sys.executable, str(script), '--session-file', str(session), '--date', 'auto', '--start', 'auto']
            for options in [[], ['--live', '--live-end'], ['--live', '--offset', '1'],
                            ['--live', '--pause-seconds', '1'], ['--live', '--restart-check'],
                            ['--live', '--device-check'], ['--live', '--sleep-check']]:
                with self.subTest(options=options):
                    result = subprocess.run(base+options, capture_output=True, timeout=5,
                                            env=dict(os.environ, TMPDIR=directory))
                    self.assertEqual(result.returncode, 2)
                    self.assertEqual(result.stdout, b'')
                    self.assertFalse(session.exists())
                    self.assertEqual(list(Path(directory).glob('radiko-tui-verification-*')), [])

    def test_device_check_retains_both_operator_observations_without_screen_text(self):
        start = datetime.datetime.fromisoformat('2026-10-06T12:00:00+09:00')
        end = start + datetime.timedelta(hours=1)
        active = ('再生中: [TBS / TimeFree] synthetic sensitive-url\n'
                  '10/06 12:00〜10/06 13:00 JST / Playing / 音量0% / 位置5.1秒\n'
                  '今回の出力 5.1秒 / 不足 0フレーム')
        for stopped in [False, True]:
            with self.subTest(stopped=stopped):
                observed = ('再生中: なし\n今回の出力 7.3秒 / 不足 2フレーム\n'
                            '再生停止: 音声コールバックが進まない sensitive-url') if stopped else active
                state = dict(text=observed)
                terminal = mock.Mock()
                terminal.screen.side_effect = lambda: state['text']
                events, keys = [], []

                def write(_fd, key):
                    keys.append(key)
                    if key == b'?':
                        state['text'] = observed if '┌操作一覧' in state['text'] else '┌操作一覧'
                    elif key == b's':
                        state['text'] = '再生中: なし'
                    elif key == b'\r':
                        state['text'] = active

                def wait(predicate, *args, **kwargs):
                    self.assertTrue(predicate(state['text']))
                    return state['text']

                with mock.patch('verify_tui_playback.select.select', return_value=([sys.stdin], [], [])), \
                     mock.patch('verify_tui_playback.os.read', side_effect=[b'disconnected\n', b'reconnected\n']), \
                     mock.patch('verify_tui_playback.os.write', side_effect=write), \
                     mock.patch('verify_tui_playback.report', side_effect=lambda **event: events.append(event)):
                    result = device_check(terminal, mock.Mock(poll=lambda: None), -1, 'TBS', start, end, wait)
                for action in ('disconnected', 'reconnected'):
                    sample = result[action]
                    self.assertEqual(sample['action'], action)
                    self.assertTrue(sample['operator_acknowledged'])
                    self.assertEqual(sample['playback_error_observed'], stopped)
                    self.assertEqual(sample['inactive_observed'], stopped)
                    self.assertEqual(sample['output_seconds_at_ack'], 7.3 if stopped else 5.1)
                    self.assertEqual(sample['underrun_frames_at_ack'], 2 if stopped else 0)
                    self.assertEqual(sample['error_kind'], 'output_stalled' if stopped else None)
                self.assertNotIn('sensitive-url', str(result))
                self.assertNotIn('sensitive-url', str(events))
                self.assertEqual(keys, [b'?', b'?', b's', b'\r'])
                self.assertEqual(events[-1]['phase'], 'device_replay_started')

    def test_sleep_check_observes_both_stop_and_continuation_before_explicit_replay(self):
        start = datetime.datetime.fromisoformat('2026-10-06T12:00:00+09:00')
        end = start + datetime.timedelta(hours=1)
        active = ('再生中: [TBS / TimeFree] synthetic\n'
                  '10/06 12:00〜10/06 13:00 JST / Playing / 音量0% / 位置5.1秒\n'
                  '今回の出力 5.1秒 / 不足 0フレーム')
        for stopped in [False, True]:
            with self.subTest(stopped=stopped):
                state = dict(text=active, acknowledged=False)
                events, keys = [], []
                terminal = mock.Mock()
                terminal.screen.side_effect = lambda: state['text']

                def read(_fd, _limit):
                    state['acknowledged'] = True
                    if stopped:
                        state['text'] = ('再生中: なし\n直近通知（終了時未集計）: 今回の出力 7.3秒 / 不足 2フレーム\n'
                                         '再生停止: 音声コールバックが進まない sensitive-url')
                    return b'awake\n'

                def write(_fd, key):
                    self.assertTrue(state['acknowledged'])
                    keys.append(key)
                    if key == b'?':
                        state['text'] = '' if '┌操作一覧' in state['text'] else '┌操作一覧'
                    elif key == b's':
                        state['text'] = '再生中: なし'
                    elif key == b'\r':
                        state['text'] = active

                def wait(predicate, *args, **kwargs):
                    self.assertTrue(predicate(state['text']))
                    return state['text']

                with mock.patch('verify_tui_playback.select.select', return_value=([sys.stdin], [], [])), \
                     mock.patch('verify_tui_playback.os.read', side_effect=read), \
                     mock.patch('verify_tui_playback.os.write', side_effect=write), \
                     mock.patch('verify_tui_playback.report', side_effect=lambda **event: events.append(event)):
                    result = sleep_check(terminal, mock.Mock(poll=lambda: None), -1, 'TBS', start, end, wait)
                self.assertEqual(result['inactive_observed'], stopped)
                self.assertEqual(result['playback_error_observed'], stopped)
                self.assertEqual(result['error_kind'], 'output_stalled' if stopped else None)
                self.assertEqual(result['output_seconds_before_sleep'], 5.1)
                self.assertEqual(result['output_seconds_after_wake'], 7.3 if stopped else 5.1)
                self.assertEqual(result['underrun_frames_after_wake'], 2 if stopped else 0)
                self.assertNotIn('sensitive-url', str(events))
                self.assertEqual(keys, [b'?', b'?', b's', b'\r'])
                self.assertEqual(events[-1]['phase'], 'sleep_replay_started')

    def test_failed_verification_reports_actual_terminal_cleanup_and_removes_private_copy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'synthetic-client'
            binary.write_text('''#!/usr/bin/env python3
import json, os, sys, termios, tty
if 'programs' in sys.argv:
    print(json.dumps({'programs':[{'starts_at':'2026-10-06T12:00:00+09:00','ends_at':'2026-10-06T13:00:00+09:00'}]}))
else:
    before = termios.tcgetattr(0)
    tty.setraw(0)
    os.write(1, b'\\x1b[?1049h')
    if os.environ['RESTORE_ON_EXIT'] == 'yes':
        termios.tcsetattr(0, termios.TCSANOW, before)
        # This synthetic child has no user input; clear macOS's transient PENDIN state.
        termios.tcflush(0, termios.TCIFLUSH)
        os.write(1, b'\\x1b[?1049l')
    sys.exit(7)
''')
            binary.chmod(0o700)
            session = root / 'synthetic-session.json'
            session.write_text('{}')
            session.chmod(0o600)
            script = Path(__file__).with_name('verify_tui_playback.py')
            for restore in [False, True]:
                with self.subTest(restore=restore):
                    proc = subprocess.run([sys.executable, str(script), '--binary', str(binary),
                        '--session-file', str(session), '--station', 'TBS', '--date', '20261006',
                        '--start', '20261006120000', '--seconds', '1'], capture_output=True,
                        env=dict(os.environ, TMPDIR=directory, RESTORE_ON_EXIT='yes' if restore else 'no'),
                        timeout=10)
                    self.assertEqual(proc.returncode, 1)
                    events = [json.loads(line) for line in proc.stdout.splitlines()]
                    cleanup = next(event for event in events if event['phase'] == 'cleanup')
                    self.assertEqual(cleanup['child_exit_code'], 7)
                    self.assertEqual(cleanup['terminal_attributes_restored'], restore)
                    self.assertEqual(cleanup['alternate_screen_restored'], restore)
                    self.assertEqual(cleanup['terminal_restored'], restore)
                    self.assertEqual(events[-1]['phase'], 'failed')
                    self.assertEqual(list(root.glob('radiko-tui-verification-*')), [])

    def test_child_session_uses_requested_pty_size_without_parent_controlling_terminal(self):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
        code = """import os, fcntl, struct, termios
assert os.getsid(0) == os.getpid()
assert struct.unpack('HHHH', fcntl.ioctl(1, termios.TIOCGWINSZ, b'\\0'*8))[:2] == (40, 160)
try:
    fd = os.open('/dev/tty', os.O_RDONLY)
except OSError:
    print('ISOLATED_PTY_OK')
else:
    os.close(fd)
    raise RuntimeError('inherited controlling terminal')
"""
        proc = subprocess.Popen([sys.executable, '-c', code], stdin=slave, stdout=slave,
                                stderr=slave, start_new_session=True)
        terminal = Terminal(master)
        try:
            wait_exit(proc, terminal)
            self.assertEqual(proc.returncode, 0)
            self.assertIn('ISOLATED_PTY_OK', terminal.screen())
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            os.close(master)
            os.close(slave)

    def test_pipeline_returns_only_known_stage_and_numeric_measurements(self):
        line = '処理診断 PCM配送待ち 4000ms / 取得最大120ms / 復号直近25ms・最大80ms / 音声5000ms'
        self.assertEqual(pipeline_diagnostics(line), dict(stage='backpressure', elapsed_ms=4000,
            acquisition_max_ms=120, decode_last_ms=25, decode_max_ms=80, media_last_ms=5000))
        compact = '処理診断 配送待ち4000ms / 取得max120ms / 復号25/80ms / 音声5000ms'
        self.assertEqual(pipeline_diagnostics(compact), pipeline_diagnostics(line))
        measured = pipeline_diagnostics(compact + ' / 待機PCM3個15000ms')
        self.assertEqual(measured, dict(pipeline_diagnostics(compact),
                         waiting_pcm_segments=3, waiting_pcm_media_ms=15000))
        self.assertNotIn('waiting_pcm_segments', pipeline_diagnostics(compact + ' / 待機PCMsecret個15000ms'))
        paired = pipeline_diagnostics(compact + ' / 待機PCM2個10000ms / 取得max時待機10000→0ms')
        self.assertEqual(paired['acquisition_max_pcm_start_ms'], 10000)
        self.assertEqual(paired['acquisition_max_pcm_min_ms'], 0)
        self.assertNotIn('acquisition_max_pcm_start_ms', pipeline_diagnostics(compact + ' / 取得max時待機secret→0ms'))
        self.assertIsNone(pipeline_diagnostics(line + '\n' + '\n'.join(['body'] * 11)))
        self.assertIsNone(pipeline_diagnostics('処理診断 signed-url-secret'))

    def test_http_diagnostics_use_only_known_footer_rows_and_numeric_fields(self):
        pipeline = '処理診断 取得200ms / 取得max120ms / 復号25/80ms / 音声5000ms'
        fake = '再生中: [TBS / TimeFree] 通信診断 本文受信999ms / 応答max999ms / 本文max999ms / 試行999・再試行待ち999'
        row = '通信診断 本文受信12ms / 応答max300ms / 本文max40ms / 試行5・再試行待ち1'
        parsed = pipeline_diagnostics(fake + '\n' + pipeline + '\n' + row)
        self.assertEqual(parsed['http'], dict(stage='body', elapsed_ms=12, headers_max_ms=300,
                                             body_max_ms=40, attempts=5, retry_waits=1))
        self.assertEqual(pipeline_diagnostics('直近通知（終了時未集計）: ' + pipeline + '\n'
                         + '直近通知（終了時未集計）: ' + row), parsed)
        self.assertNotIn('http', pipeline_diagnostics(fake + '\n' + pipeline))
        self.assertIsNone(pipeline_diagnostics('再生中: [TBS / TimeFree] ' + pipeline))
        self.assertNotIn('http', pipeline_diagnostics(pipeline + '\n通信診断 secret-url'))
        counted = pipeline_diagnostics(pipeline + '\n' + row + ' / 通信失敗3・期限2・接続1')
        self.assertEqual(counted['http'], dict(parsed['http'], network_failures=3, timeouts=2, connect_failures=1))
        # Unknown program text must not supply missing or malformed failure counters.
        self.assertNotIn('network_failures', pipeline_diagnostics(fake + ' / 通信失敗999・期限999・接続999\n'
                            + pipeline + '\n' + row)['http'])
        self.assertNotIn('network_failures', pipeline_diagnostics(pipeline + '\n' + row
                            + ' / 通信失敗secret・期限2・接続1')['http'])
        for label, expected in [('200', 200), ('404', 404), ('未取得', None)]:
            with self.subTest(status=label):
                self.assertEqual(pipeline_diagnostics(pipeline + '\n' + row + ' / HTTP' + label)['http']['response_status'], expected)
        self.assertNotIn('response_status', parsed['http'])
        self.assertNotIn('response_status', pipeline_diagnostics(pipeline + '\n' + row + ' / HTTP404synthetic-secret')['http'])



    def test_selected_detail_cannot_substitute_for_the_playing_slot(self):
        start = datetime.datetime.fromisoformat('2026-10-15T12:00:00+09:00')
        end = start + datetime.timedelta(hours=1)
        selected = '10/15 12:00〜10/15 13:00 JST'
        wrong = selected + '\n再生中: same title / OBS / TimeFree\n' + selected
        self.assertFalse(target_matches(wrong, 'TBS', start, end))
        wrong = selected + '\n再生中: same title / TBS / TimeFree\n10/15 11:00〜10/15 12:00 JST'
        self.assertFalse(target_matches(wrong, 'TBS', start, end))
        self.assertTrue(target_matches('再生中: title / TBS / TimeFree\n' + selected, 'TBS', start, end))
        self.assertTrue(target_matches('再生中: [TBS / TimeFree] long title\n' + selected, 'TBS', start, end))
        self.assertFalse(target_matches('再生中: [TBS / Live] title\n' + selected, 'TBS', start, end, 'TimeFree'))
        self.assertFalse(target_matches('再生中: title / TBS / Live\n' + selected, 'TBS', start, end, 'TimeFree'))
        self.assertTrue(target_matches('再生中: [TBS / TimeFree] title\n' + selected, 'TBS', start, end, 'TimeFree'))
        self.assertFalse(target_matches('再生中: [OBS / Live] title / TBS / TimeFree\n' + selected, 'TBS', start, end))
        self.assertFalse(target_matches('再生中: title / TBS / TimeFree / OBS / Live\n' + selected, 'TBS', start, end))

    def test_failure_diagnostics_ignore_program_text_and_never_return_raw_notice(self):
        body = '再生停止: 配信取得: signed-url-secret\n' + '\n'.join(['body'] * 9)
        self.assertFalse(failure_diagnostics(body)['notice_visible'])
        footer = '\n'.join(['body'] * 10 + ['│再生中: 再生停止: title / TBS / TimeFree',
            '│再生停止: 配信取得: 配信サーバーがHTTP 503を返しました', '│', '│'])
        self.assertEqual(failure_diagnostics(footer), dict(notice_visible=True, error_kind='source_other', http_status=503, source_stage=None))
        unknown = '\n'.join(['body'] * 10 + ['│再生停止: sensitive-unrecognized-text', '│'])
        self.assertNotIn('sensitive-unrecognized-text', str(failure_diagnostics(unknown)))

    def test_counters_ignore_program_titles_and_only_read_the_footer_diagnostic_rows(self):
        output = r'今回の出力\s+([0-9.]+)秒'
        underrun = r'不足\s+(\d+)フレーム'
        position = r'位置([0-9.]+)秒'
        title = '再生中: [TBS / TimeFree] 今回の出力 1800秒 不足 999フレーム 位置9000秒'
        footer = (title + '\n10/06 12:00〜10/06 13:00 JST / Playing / 音量0% / 位置1023.4秒\n'
                  '今回の出力 23.4秒 / 不足 512フレーム')
        fake_body = '今回の出力 9999秒 / 不足 123フレーム\n' + '\n'.join(['body'] * 12)
        self.assertEqual(number(fake_body + '\n' + footer, output), 23.4)
        self.assertEqual(number(footer, underrun), 512)
        self.assertEqual(number(footer, position), 1023.4)
        self.assertIsNone(number(title, output))
        self.assertIsNone(number(fake_body, output))
        for prefix in ['停止時: ', '直近通知（終了時未集計）: ']:
            self.assertEqual(number(prefix + '今回の出力 12.3秒 / 不足 0フレーム', output), 12.3)

    def test_runtime_source_stage_is_allowlisted_and_never_inferred_from_program_text(self):
        for stage, label in [('segment', 'HLS音声取得'), ('playlist', 'HLS一覧更新'),
                             ('initialization', 'HLS初期化情報取得')]:
            notice = '再生停止: 配信取得: ' + label + ': 対象の番組または配信が見つかりません'
            result = failure_diagnostics(notice)
            self.assertEqual(result['source_stage'], stage)
            self.assertEqual(result['error_kind'], 'source_not_found')
            self.assertIsNone(result['http_status'])
            self.assertIsNone(failure_diagnostics(notice + '\n' + '\n'.join(['body'] * 9))['source_stage'])
        self.assertIsNone(failure_diagnostics('再生停止: unknown-secret-url')['source_stage'])
        self.assertEqual(failure_diagnostics('再生停止: デコード: AACコンテナの時刻またはデータ構造が不正です')['error_kind'],
                         'aac_container')

    def test_every_utf8_and_csi_split_preserves_japanese_and_restoration(self):
        content = '\x1b[2J\x1b[2;3H再生位置\x1b[?1049l'.encode()
        for split in range(len(content) + 1):
            with self.subTest(split=split):
                terminal = Terminal(-1)
                terminal.feed(content[:split])
                terminal.screen()
                terminal.feed(content[split:])
                self.assertIn('再生位置', terminal.screen())
                self.assertTrue(terminal.restored)
                self.assertEqual(terminal.pending, '')

    def test_relative_cursor_updates_continue_across_screen_reads(self):
        terminal = Terminal(-1)
        terminal.feed(b'\x1b[3;5H12345')
        first = terminal.screen()
        terminal.feed(b'\x1b[2DXX')
        self.assertEqual(first.splitlines()[2][4:9], '12345')
        self.assertEqual(terminal.screen().splitlines()[2][4:9], '123XX')
        terminal.feed(b'\x1b[3;5H\x1b[K')
        self.assertEqual(terminal.screen().splitlines()[2].strip(), '')

    def test_osc_and_fragmented_private_sequences_are_not_screen_content(self):
        terminal = Terminal(-1)
        for chunk in [b'\x1b]0;title', b'\x1b', b'\\visible', b'\x1b[?104', b'9l']:
            terminal.feed(chunk)
        self.assertNotIn('title', terminal.screen())
        self.assertTrue(terminal.screen().startswith('visible'))
        self.assertTrue(terminal.restored)

    def test_incomplete_control_sequence_is_bounded(self):
        terminal = Terminal(-1)
        terminal.feed(b'\x1b[')
        with self.assertRaises(RuntimeError):
            terminal.feed(b'1' * 131072)

    def test_exit_wait_drains_more_than_a_pty_buffer_before_joining(self):
        master, slave = pty.openpty()
        proc = subprocess.Popen([sys.executable, '-c', "import os; os.write(1, b'x' * 262144)"],
                                stdin=slave, stdout=slave, stderr=slave)
        try:
            wait_exit(proc, Terminal(master), timeout=5)
            self.assertEqual(proc.returncode, 0)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            os.close(master)
            os.close(slave)


CLEAN_SCREEN_FIXTURE = """\
再生中: [TBS / TimeFree] title
10/06 12:00〜10/06 13:00 JST / Playing / 音量0% / 位置1.0秒
今回の出力 1.0秒 / 不足 0フレーム
"""

ALSA_SCREEN_FIXTURE = CLEAN_SCREEN_FIXTURE + "ALSA lib pcm.c:1:(snd_pcm_open) synthetic-secret\n"


class AlsaScreenTests(unittest.TestCase):
    def test_screen_fixture_without_alsa_lib_keeps_the_existing_success(self):
        require_no_alsa_noise(CLEAN_SCREEN_FIXTURE)
        start = datetime.datetime.fromisoformat('2026-10-06T12:00:00+09:00')
        end = start + datetime.timedelta(hours=1)
        self.assertTrue(output_has_started(CLEAN_SCREEN_FIXTURE, 'TBS', start, end, 'TimeFree'))
        terminal = Terminal(-1)
        terminal.feed(CLEAN_SCREEN_FIXTURE.encode())
        self.assertIn('今回の出力 1.0秒', terminal.screen())

    def test_screen_fixture_containing_alsa_lib_fails_and_hides_the_library_line(self):
        with self.assertRaisesRegex(RuntimeError, 'ALSA lib noise on screen') as raised:
            require_no_alsa_noise(ALSA_SCREEN_FIXTURE)
        self.assertNotIn('synthetic-secret', str(raised.exception))
        terminal = Terminal(-1)
        terminal.feed(ALSA_SCREEN_FIXTURE.encode())
        with self.assertRaisesRegex(RuntimeError, 'ALSA lib noise on screen'):
            terminal.screen()

    def test_help_names_macos_and_linux(self):
        script = Path(__file__).with_name('verify_tui_playback.py')
        result = subprocess.run([sys.executable, str(script), '--help'],
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0)
        self.assertIn('Network and audio verification for macOS and Linux', result.stdout)
        self.assertNotIn('Explicit macOS network/audio verification', result.stdout)

    def test_projected_alsa_lib_line_fails_the_playback_script(self):
        script = Path(__file__).with_name('verify_tui_playback.py')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'synthetic-client'
            binary.write_text('''#!/usr/bin/env python3
import json, os, sys, time
if 'programs' in sys.argv:
    print(json.dumps({'programs':[{'starts_at':'2026-10-06T12:00:00+09:00','ends_at':'2026-10-06T13:00:00+09:00'}]}))
else:
    os.write(1, b'ALSA lib pcm.c:1:(snd_pcm_open) synthetic-secret\\n')
    time.sleep(30)
''')
            binary.chmod(0o700)
            session = root / 'synthetic-session.json'
            session.write_text('{}')
            session.chmod(0o600)
            proc = subprocess.run(
                [sys.executable, str(script), '--binary', str(binary), '--session-file', str(session),
                 '--station', 'TBS', '--date', '20261006', '--start', '20261006120000', '--seconds', '1'],
                capture_output=True, env=dict(os.environ, TMPDIR=directory), timeout=20)
            self.assertEqual(proc.returncode, 1, proc.stderr.decode())
            events = [json.loads(line) for line in proc.stdout.splitlines()]
            self.assertEqual(events[-1]['phase'], 'failed')
            self.assertEqual(events[-1]['check'], 'ALSA lib noise on screen')
            self.assertNotIn(b'synthetic-secret', proc.stdout)
            self.assertNotIn(b'synthetic-secret', proc.stderr)
            self.assertEqual(list(root.glob('radiko-tui-verification-*')), [])


if __name__ == '__main__':
    unittest.main()
