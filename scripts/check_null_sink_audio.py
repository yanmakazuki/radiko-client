#!/usr/bin/env python3
"""Read null-sink CI logs and fail on ALSA or JACK diagnostics or unexpected underruns.

The CLI lines this accepts are the ones printed today:

  output-info success:
    {rate}Hz / {format} / {channels}ch: 対応宣言あり（再生時に再確認）
  probe-output --mute progress (stderr):
    無音出力中: {frames}フレーム / {rate}Hz / {format} / 不足{underrun}フレーム
  probe-output --mute completion (stdout):
    無音出力完了: {frames}フレーム / {rate}Hz / {format} / {channels}ch / バッファ不足: {underrun}フレーム

Native summaries come from the ignored tests in player/playback.rs:

  synthetic_fragmented_muted ... underrun_frames={n}
  synthetic_muted gap_after={1|4} ... underrun_frames={n}
  test result: ok. N passed; N failed; N ignored;

gap_after=1 is the case that must observe underruns. A non-zero count there is
not a failure. gap_after=4 and the fragmented summary must be zero.
"""

import argparse
import re
import sys

_ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
_UNDERRUN = re.compile(r"不足\s*[:：]?\s*([0-9]+(?:,[0-9]{3})*)\s*フレーム")
_COMPLETE = re.compile(r"無音出力完了\s*[:：]\s*([0-9]+(?:,[0-9]{3})*)\s*フレーム")
_RESULT = re.compile(
    r"test result:\s+ok\.\s+([0-9]+)\s+passed;\s+([0-9]+)\s+failed;\s+([0-9]+)\s+ignored\b"
)
_FRAGMENTED = re.compile(
    r"synthetic_fragmented_muted\b[^\n]*underrun_frames=([0-9]+)"
)
_GAP = re.compile(
    r"synthetic_muted\s+gap_after=([0-9]+)\b[^\n]*underrun_frames=([0-9]+)"
)


class CheckError(Exception):
    pass


def normalize(text):
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    return _ANSI.sub("", text)


_JACK_MARKERS = (
    "Cannot connect to server socket",
    "Cannot connect to server request channel",
    "jack server is not running",
    "JackShmReadWritePtr",
)


def alsa_lib_lines(text):
    return [line for line in normalize(text).split("\n") if "ALSA lib" in line]


def jack_lines(text):
    return [
        line
        for line in normalize(text).split("\n")
        if any(marker in line for marker in _JACK_MARKERS)
    ]


def _count(raw):
    return int(raw.replace(",", ""))


def underrun_reports(text):
    """Return (frame count, source line) for every 不足 / バッファ不足 report."""
    found = []
    for line in normalize(text).split("\n"):
        if "不足" not in line:
            continue
        counts = _UNDERRUN.findall(line)
        if not counts:
            raise CheckError(f"could not read an underrun count from: {line}")
        found.extend((_count(raw), line) for raw in counts)
    return found


def completion_frames(text):
    return [_count(raw) for raw in _COMPLETE.findall(normalize(text))]


def _require_no_alsa(label, stdout, stderr, errors):
    for stream, text in (("stdout", stdout), ("stderr", stderr)):
        for line in alsa_lib_lines(text):
            errors.append(f"{label} {stream} contains an ALSA lib line: {line}")
        for line in jack_lines(text):
            errors.append(f"{label} {stream} contains a JACK line: {line}")


def check_output_info(status, stdout, stderr):
    errors = []
    if status != 0:
        errors.append(f"output-info exited {status}")
    _require_no_alsa("output-info", stdout, stderr, errors)
    if "対応宣言あり" not in normalize(stdout):
        errors.append("output-info stdout has no 対応宣言あり line")
    if errors:
        raise CheckError("\n".join(errors))
    alsa = len(alsa_lib_lines(stdout)) + len(alsa_lib_lines(stderr))
    jack = len(jack_lines(stdout)) + len(jack_lines(stderr))
    return f"cli-output-info status={status} alsa_lib_lines={alsa} jack_lines={jack}"


def check_probe(status, stdout, stderr, elapsed, min_elapsed):
    errors = []
    if status != 0:
        errors.append(f"probe-output exited {status}")
    _require_no_alsa("probe-output", stdout, stderr, errors)
    try:
        reports = underrun_reports(stdout) + underrun_reports(stderr)
    except CheckError as exc:
        reports = []
        errors.append(str(exc))
    if not reports:
        errors.append("probe-output printed no 不足 / バッファ不足 count")
    for count, line in reports:
        if count != 0:
            errors.append(f"probe-output underrun frames {count} (expected 0): {line}")
    frames = completion_frames(stdout)
    if len(frames) != 1:
        errors.append(
            f"probe-output stdout has {len(frames)} 無音出力完了 lines (expected 1)"
        )
    elif frames[0] <= 0:
        errors.append(f"probe-output completion frames {frames[0]} (expected > 0)")
    if elapsed is not None and elapsed < min_elapsed:
        errors.append(
            f"probe-output finished in {elapsed:.3f}s, under the {min_elapsed:.3f}s "
            "realtime floor (ALSA's null PCM advances faster than realtime)"
        )
    if errors:
        raise CheckError("\n".join(errors))
    counts = ",".join(str(count) for count, _line in reports)
    played = frames[0]
    elapsed_text = "none" if elapsed is None else f"{elapsed:.3f}"
    return (
        "cli-probe status={status} underrun_frames={counts} "
        "frames={played} elapsed_s={elapsed_text}"
    ).format(status=status, counts=counts, played=played, elapsed_text=elapsed_text)


def check_native(status, stdout, stderr):
    errors = []
    if status != 0:
        errors.append(f"native tests exited {status}")
    _require_no_alsa("native tests", stdout, stderr, errors)
    text = normalize(stdout + "\n" + stderr)
    results = _RESULT.findall(text)
    if len(results) != 1:
        errors.append(f"expected one 'test result: ok' line, found {len(results)}")
        passed = failed = ignored = None
    else:
        passed, failed, ignored = (int(value) for value in results[0])
        if (passed, failed, ignored) != (2, 0, 0):
            errors.append(
                f"native test result passed={passed} failed={failed} ignored={ignored} "
                "(expected 2 passed, 0 failed, 0 ignored)"
            )
    fragmented = [int(value) for value in _FRAGMENTED.findall(text)]
    if fragmented != [0]:
        errors.append(
            f"synthetic_fragmented_muted underrun_frames={fragmented} (expected [0])"
        )
    gaps = {}
    for gap_after, underrun in _GAP.findall(text):
        gaps.setdefault(int(gap_after), []).append(int(underrun))
    gap1 = gaps.get(1, [])
    gap4 = gaps.get(4, [])
    if gap4 != [0]:
        errors.append(f"gap_after=4 underrun_frames={gap4} (expected [0])")
    if len(gap1) != 1 or gap1[0] <= 0:
        errors.append(
            f"gap_after=1 underrun_frames={gap1} "
            "(expected one count greater than 0; that case observes underruns)"
        )
    extra = sorted(gap for gap in gaps if gap not in (1, 4))
    if extra:
        errors.append(f"unexpected synthetic_muted gap_after values: {extra}")
    if errors:
        raise CheckError("\n".join(errors))
    return (
        f"native-summary passed={passed} failed={failed} ignored={ignored} "
        f"fragmented_underrun_frames={fragmented[0]} "
        f"gap_after_4_underrun_frames={gap4[0]} "
        f"gap_after_1_underrun_frames={gap1[0]}"
    )


def _read(path):
    with open(path, encoding="utf-8", errors="replace") as handle:
        return handle.read()


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    output_info = sub.add_parser("output-info")
    output_info.add_argument("--status", type=int, required=True)
    output_info.add_argument("--stdout", required=True)
    output_info.add_argument("--stderr", required=True)

    probe = sub.add_parser("probe")
    probe.add_argument("--status", type=int, required=True)
    probe.add_argument("--stdout", required=True)
    probe.add_argument("--stderr", required=True)
    probe.add_argument("--elapsed", type=float, required=True)
    probe.add_argument("--min-elapsed", type=float, default=2.5)

    native = sub.add_parser("native")
    native.add_argument("--status", type=int, required=True)
    native.add_argument("--stdout", required=True)
    native.add_argument("--stderr", required=True)

    args = parser.parse_args(argv)
    try:
        if args.command == "output-info":
            summary = check_output_info(args.status, _read(args.stdout), _read(args.stderr))
        elif args.command == "probe":
            summary = check_probe(
                args.status,
                _read(args.stdout),
                _read(args.stderr),
                args.elapsed,
                args.min_elapsed,
            )
        else:
            summary = check_native(args.status, _read(args.stdout), _read(args.stderr))
    except CheckError as exc:
        print(exc, file=sys.stderr)
        return 1
    print(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
