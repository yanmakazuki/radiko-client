"""Offline checks for null-sink log parsing. No audio device and no network."""

import unittest

from check_null_sink_audio import (
    CheckError,
    check_native,
    check_output_info,
    check_probe,
)


def probe_streams(frames=144000, underrun=0, rate=48000, sample_format="f32", channels=2, done=True):
    progress = (
        f"無音出力中: {frames}フレーム / {rate}Hz / {sample_format} / 不足{underrun}フレーム\n"
    )
    state = "完了" if done else "中断"
    final = (
        f"無音出力{state}: {frames}フレーム / {rate}Hz / {sample_format} / "
        f"{channels}ch / バッファ不足: {underrun}フレーム\n"
    )
    return final, progress


OUTPUT_INFO_STDOUT = "現在の既定出力機器: default\n48000Hz / f32 / 2ch: 対応宣言あり（再生時に再確認）\n"


class OutputInfoTests(unittest.TestCase):
    def test_success_allows_empty_stderr(self):
        summary = check_output_info(0, OUTPUT_INFO_STDOUT, "")
        self.assertIn("status=0", summary)
        self.assertIn("alsa_lib_lines=0", summary)

    def test_alsa_lib_anywhere_in_a_stderr_line_fails(self):
        stderr = "note: ALSA lib confmisc.c:855:(parse_card) cannot find card '0'\n"
        with self.assertRaises(CheckError) as raised:
            check_output_info(0, OUTPUT_INFO_STDOUT, stderr)
        self.assertIn("ALSA lib", str(raised.exception))

    def test_ansi_wrapped_alsa_lib_line_fails(self):
        stderr = "\x1b[31mALSA lib pcm.c:1:(snd_pcm_open) error\x1b[0m\n"
        with self.assertRaises(CheckError):
            check_output_info(0, OUTPUT_INFO_STDOUT, stderr)

    def test_missing_supported_format_line_fails(self):
        with self.assertRaises(CheckError) as raised:
            check_output_info(0, "現在の既定出力機器: default\n", "")
        self.assertIn("対応宣言あり", str(raised.exception))

    def test_nonzero_status_fails(self):
        with self.assertRaises(CheckError):
            check_output_info(1, OUTPUT_INFO_STDOUT, "")


class ProbeTests(unittest.TestCase):
    def test_current_mute_format_with_zero_underruns_passes(self):
        stdout, stderr = probe_streams()
        summary = check_probe(0, stdout, stderr, elapsed=5.2, min_elapsed=2.5)
        self.assertIn("underrun_frames=0,0", summary)
        self.assertIn("frames=144000", summary)

    def test_spaced_and_comma_grouped_counts_are_read(self):
        stderr = "無音出力中: 1,440 フレーム / 48000Hz / f32 / 不足 0 フレーム\n"
        stdout = "無音出力完了: 144,000フレーム / 48000Hz / f32 / 2ch / バッファ不足：0フレーム\n"
        summary = check_probe(0, stdout, stderr, elapsed=3.1, min_elapsed=2.5)
        self.assertIn("underrun_frames=0,0", summary)
        self.assertIn("frames=144000", summary)

    def test_nonzero_progress_underrun_fails(self):
        stdout, stderr = probe_streams()
        stderr = stderr.replace("不足0フレーム", "不足12フレーム")
        with self.assertRaises(CheckError) as raised:
            check_probe(0, stdout, stderr, elapsed=5.2, min_elapsed=2.5)
        self.assertIn("12", str(raised.exception))

    def test_nonzero_completion_underrun_fails(self):
        stdout, stderr = probe_streams(underrun=8)
        with self.assertRaises(CheckError) as raised:
            check_probe(0, stdout, stderr, elapsed=5.2, min_elapsed=2.5)
        self.assertIn("8", str(raised.exception))

    def test_interrupt_without_completion_fails(self):
        stdout, stderr = probe_streams(done=False)
        with self.assertRaises(CheckError) as raised:
            check_probe(0, stdout, stderr, elapsed=5.2, min_elapsed=2.5)
        self.assertIn("無音出力完了", str(raised.exception))

    def test_unreadable_underrun_label_fails(self):
        stdout, _stderr = probe_streams()
        stderr = "無音出力中: 不足なし\n"
        with self.assertRaises(CheckError) as raised:
            check_probe(0, stdout, stderr, elapsed=5.2, min_elapsed=2.5)
        self.assertIn("could not read", str(raised.exception))

    def test_faster_than_realtime_fails(self):
        stdout, stderr = probe_streams()
        with self.assertRaises(CheckError) as raised:
            check_probe(0, stdout, stderr, elapsed=0.4, min_elapsed=2.5)
        self.assertIn("realtime", str(raised.exception))

    def test_alsa_lib_on_stdout_fails(self):
        stdout, stderr = probe_streams()
        stdout += "ALSA lib pcm_pulse.c:1: error\n"
        with self.assertRaises(CheckError):
            check_probe(0, stdout, stderr, elapsed=5.2, min_elapsed=2.5)


NATIVE_OK = """\
synthetic_muted gap_after=1 gate_frames=57766 underrun_frames=343221 wall_ms=21313
synthetic_muted gap_after=4 gate_frames=56621 underrun_frames=0 wall_ms=27181
synthetic_fragmented_progress frames=0 underrun_frames=0
synthetic_fragmented_muted seconds=5 frames=240000 rate=48000 channels=2 underrun_frames=0 wall_ms=7164
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 178 filtered out; finished in 55.68s
"""


class NativeTests(unittest.TestCase):
    def test_two_successes_keep_the_expected_gap_underrun(self):
        summary = check_native(0, NATIVE_OK, "")
        self.assertIn("passed=2", summary)
        self.assertIn("fragmented_underrun_frames=0", summary)
        self.assertIn("gap_after_4_underrun_frames=0", summary)
        self.assertIn("gap_after_1_underrun_frames=343221", summary)

    def test_colored_harness_line_still_parses(self):
        colored = NATIVE_OK.replace(
            "test result: ok. 2 passed;",
            "\x1b[32mtest result: ok. 2 passed;\x1b[0m",
        )
        summary = check_native(0, colored, "")
        self.assertIn("passed=2", summary)

    def test_gap_after_4_underrun_fails(self):
        text = NATIVE_OK.replace("gap_after=4 gate_frames=56621 underrun_frames=0", "gap_after=4 gate_frames=56621 underrun_frames=10")
        with self.assertRaises(CheckError) as raised:
            check_native(0, text, "")
        self.assertIn("gap_after=4", str(raised.exception))

    def test_fragmented_underrun_fails(self):
        text = NATIVE_OK.replace(
            "synthetic_fragmented_muted seconds=5 frames=240000 rate=48000 channels=2 underrun_frames=0",
            "synthetic_fragmented_muted seconds=5 frames=240000 rate=48000 channels=2 underrun_frames=4",
        )
        with self.assertRaises(CheckError) as raised:
            check_native(0, text, "")
        self.assertIn("synthetic_fragmented_muted", str(raised.exception))

    def test_gap_after_1_zero_fails(self):
        text = NATIVE_OK.replace("gap_after=1 gate_frames=57766 underrun_frames=343221", "gap_after=1 gate_frames=57766 underrun_frames=0")
        with self.assertRaises(CheckError) as raised:
            check_native(0, text, "")
        self.assertIn("gap_after=1", str(raised.exception))

    def test_single_pass_fails(self):
        text = NATIVE_OK.replace("2 passed; 0 failed; 0 ignored", "1 passed; 0 failed; 1 ignored")
        with self.assertRaises(CheckError) as raised:
            check_native(0, text, "")
        self.assertIn("passed=1", str(raised.exception))

    def test_alsa_lib_on_stderr_fails_even_when_tests_pass(self):
        with self.assertRaises(CheckError) as raised:
            check_native(0, NATIVE_OK, "ALSA lib pcm.c:1: boom\n")
        self.assertIn("ALSA lib", str(raised.exception))

    def test_nonzero_exit_fails(self):
        with self.assertRaises(CheckError) as raised:
            check_native(101, NATIVE_OK, "")
        self.assertIn("exited 101", str(raised.exception))


if __name__ == "__main__":
    unittest.main()
