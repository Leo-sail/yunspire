#!/usr/bin/env python3
"""Yunspire-owned audio preparation, segmentation, and transcript merging."""
import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def emit(payload):
    json.dump(payload, sys.stdout, ensure_ascii=False)


def run(command, timeout):
    options = {
        "capture_output": True,
        "text": True,
        "encoding": "utf-8",
        "errors": "strict",
        "timeout": timeout,
    }
    if sys.platform == "win32":
        options["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    try:
        return subprocess.run(command, **options)
    except (OSError, UnicodeError, subprocess.TimeoutExpired) as exc:
        return subprocess.CompletedProcess(command, 1, "", str(exc))


def validate_windows_payload(payload):
    if not isinstance(payload, dict) or payload.get("schema") != "yunspire.windows-speech.v1":
        raise ValueError("windows_speech_adapter_schema_mismatch")
    transcript = payload.get("transcript")
    segments = payload.get("segments")
    warnings = payload.get("warnings")
    errors = payload.get("errors")
    if not isinstance(transcript, str) or not all(isinstance(value, list) for value in (segments, warnings, errors)):
        raise ValueError("windows_speech_adapter_contract_invalid")
    if any(not isinstance(value, str) for value in warnings + errors):
        raise ValueError("windows_speech_diagnostics_invalid")
    on_device = payload.get("on_device")
    if not isinstance(on_device, bool):
        raise ValueError("windows_speech_on_device_state_invalid")
    previous_start = 0
    for segment in segments:
        if not isinstance(segment, dict) or not isinstance(segment.get("text"), str):
            raise ValueError("windows_speech_segment_invalid")
        start = segment.get("start_ms")
        end = segment.get("end_ms")
        if not isinstance(start, int) or not isinstance(end, int) or start < previous_start or end < start:
            raise ValueError("windows_speech_timestamps_invalid")
        previous_start = start
    if transcript and not segments:
        raise ValueError("windows_speech_segments_missing")
    if not transcript and not errors:
        raise ValueError("windows_speech_empty_success")
    if transcript and on_device is not True:
        raise ValueError("windows_speech_not_on_device")
    return payload


def windows_speech_adapter():
    configured = os.environ.get("YUNSPIRE_WINDOWS_SPEECH_ADAPTER", "").strip()
    candidates = []
    if configured:
        candidates.append(Path(configured).expanduser())
    candidates.extend(
        (
            Path(__file__).with_name("bin") / "yunspire-speech.exe",
            Path(__file__).resolve().parents[3]
            / "src-tauri"
            / "target"
            / "yunspire-native"
            / "yunspire-speech.exe",
        )
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    return None


def macos_speech_helper_app():
    configured = os.environ.get("YUNSPIRE_MACOS_SPEECH_HELPER_APP", "").strip()
    candidates = []
    if configured:
        candidates.append(Path(configured).expanduser())
    candidates.append(Path(__file__).with_name("bin") / "Yunspire Speech Helper.app")
    for candidate in candidates:
        executable = candidate / "Contents" / "MacOS" / "yunspire-speech"
        info_plist = candidate / "Contents" / "Info.plist"
        if candidate.is_dir() and executable.is_file() and info_plist.is_file():
            return candidate.resolve()
    return None


def compile_speech_adapter(source, target):
    if sys.platform != "darwin":
        return False, "当前平台不支持云枢本地语音适配器：该适配器依赖 macOS Speech Framework"
    if os.environ.get("YUNSPIRE_MACOS_ALLOW_RUNTIME_COMPILE") != "1":
        return False, "macOS 语音适配器未随安装包部署；为避免触发 Command Line Tools，云枢不会在用户设备上运行 clang"
    import shutil
    compiler = shutil.which("clang")
    if not compiler:
        return False, "本机缺少 Apple clang 编译器"
    info_plist = source.with_name("yunspire_speech_info.plist")
    target.parent.mkdir(parents=True, exist_ok=True)
    bundle_contents = target.parent.parent
    shutil.copyfile(info_plist, bundle_contents / "Info.plist")
    result = run([
        compiler,
        "-fobjc-arc",
        str(source),
        "-framework", "Speech",
        "-framework", "Foundation",
        f"-Wl,-sectcreate,__TEXT,__info_plist,{info_plist}",
        "-o", str(target),
    ], 120)
    if result.returncode != 0:
        return False, result.stderr[-800:]
    signer = Path("/usr/bin/codesign")
    if signer.is_file():
        signed = run([str(signer), "--force", "--sign", "-", str(bundle_contents.parent)], 60)
        if signed.returncode != 0:
            return False, signed.stderr[-800:]
    return True, ""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("audio")
    parser.add_argument("--locale", default="zh-CN")
    parser.add_argument("--work-dir", default="")
    args = parser.parse_args()
    audio = Path(args.audio).expanduser().resolve()
    if not audio.is_file():
        emit({"transcript": "", "segments": [], "warnings": [], "errors": ["audio_file_unavailable"]})
        return
    if sys.platform == "win32":
        helper = windows_speech_adapter()
        if helper is None:
            emit({"transcript": "", "segments": [], "warnings": ["Windows 本地语音适配器未随安装包部署；请使用完整 Yunspire 安装包重新安装"], "errors": ["windows_speech_adapter_missing"]})
            return
        result = run([str(helper), str(audio), args.locale], 720)
        try:
            if result.returncode == 0:
                emit(validate_windows_payload(json.loads(result.stdout)))
                return
            warning = (result.stderr or "").strip() or "Windows 本地语音识别进程未返回结果"
            emit({"transcript": "", "segments": [], "warnings": [warning], "errors": ["windows_speech_adapter_failed"]})
        except (json.JSONDecodeError, ValueError) as exc:
            emit({"transcript": "", "segments": [], "warnings": [str(exc) or "Windows 本地语音适配器返回无效 JSON"], "errors": ["windows_speech_adapter_invalid"]})
        return
    owned_temp = None
    if args.work_dir:
        work_dir = Path(args.work_dir).expanduser().resolve()
        work_dir.mkdir(parents=True, exist_ok=True)
    else:
        owned_temp = tempfile.TemporaryDirectory()
        work_dir = Path(owned_temp.name)
    app_bundle = macos_speech_helper_app() if sys.platform == "darwin" else None
    detail = ""
    if app_bundle is None:
        source = Path(__file__).with_name("yunspire_speech.m")
        adapter = work_dir / "Yunspire Speech Helper.app" / "Contents" / "MacOS" / "yunspire-speech"
        compiled, detail = compile_speech_adapter(source, adapter)
        if compiled:
            app_bundle = adapter.parents[2]
    if app_bundle is None:
        error = (
            "platform_speech_transcription_unavailable"
            if sys.platform != "darwin"
            else (
                "speech_adapter_build_failed"
                if os.environ.get("YUNSPIRE_MACOS_ALLOW_RUNTIME_COMPILE") == "1"
                else "macos_speech_adapter_missing"
            )
        )
        emit({"transcript": "", "segments": [], "warnings": [detail], "errors": [error]})
        return
    result_path = work_dir / "speech-result.json"
    result_path.unlink(missing_ok=True)
    result = run([
        "/usr/bin/open", "-W", "-n", str(app_bundle), "--args",
        str(audio), args.locale, str(result_path),
    ], 660)
    if result.returncode != 0:
        warning = (result.stderr or "").strip() or "本机语音识别进程未返回结果；请检查 macOS 语音识别权限"
        emit({"transcript": "", "segments": [], "warnings": [warning], "errors": ["speech_adapter_failed"]})
        return
    if not result_path.is_file():
        emit({"transcript": "", "segments": [], "warnings": ["Yunspire 语音助手没有返回结果；请检查 macOS 语音识别权限"], "errors": ["speech_adapter_no_result"]})
        return
    try:
        emit(json.loads(result_path.read_text(encoding="utf-8")))
    except json.JSONDecodeError:
        emit({"transcript": "", "segments": [], "warnings": ["语音适配器返回无效 JSON"], "errors": ["speech_adapter_invalid"]})


if __name__ == "__main__":
    main()
