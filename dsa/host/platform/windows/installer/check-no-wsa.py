#!/usr/bin/env python3
"""Checks that no Microsoft Windows Subsystem for Android (WSA) binaries or
components are present in the installer payload or image build inputs.

Desktop Subsystem for Android is an independent open-source subsystem and does
not include, wrap or distribute Microsoft WSA binaries.
"""

import argparse
import json
import os
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

# Known WSA binary and package markers
WSA_BLOCKED_NAMES = {
    "wsaclient.exe",
    "wsaservice.exe",
    "wsasettings.exe",
    "wsagpusupport.dll",
    "wsapackage.msix",
    "wsapackage.msixbundle",
    "wsainterop.dll",
    "wsadockservice.exe",
    "wsaconfig.exe",
}

WSA_FORBIDDEN_PATTERNS = [
    re.compile(r"^wsa.*\.exe$", re.IGNORECASE),
    re.compile(r"^wsa.*\.dll$", re.IGNORECASE),
    re.compile(r"^wsa.*\.msix(bundle)?$", re.IGNORECASE),
]

WSA_STRING_MARKERS = [
    b"Windows Subsystem for Android",
    b"Microsoft.Windows.SubsystemForAndroid",
    b"WsaClient",
    b"WsaService",
]


def check_image_inputs(inputs_path: Path):
    if not inputs_path.exists():
        raise FileNotFoundError(f"Image inputs not found: {inputs_path}")

    text = inputs_path.read_text(encoding="utf-8")
    lower = text.lower()
    if "windows subsystem for android" in lower or "wsaclient" in lower or "wsaservice" in lower:
        raise ValueError(f"{inputs_path} contains forbidden WSA references")

    data = json.loads(text)
    for img in data.get("images", []):
        branch = img.get("branch", "")
        target = img.get("target", "")
        if "wsa" in branch.lower() or "wsa" in target.lower():
            raise ValueError(f"Image input contains forbidden WSA target/branch: {branch} / {target}")
        for file_key, file_info in img.get("files", {}).items():
            fname = file_info.get("name", "").lower()
            if "wsa" in fname:
                raise ValueError(f"Image input contains forbidden WSA file: {fname}")


def check_manifest(manifest_path: Path):
    if not manifest_path.exists():
        raise FileNotFoundError(f"Manifest not found: {manifest_path}")

    text = manifest_path.read_text(encoding="utf-8")
    lower = text.lower()
    for blocked in WSA_BLOCKED_NAMES:
        if blocked in lower:
            raise ValueError(f"Manifest {manifest_path} references forbidden WSA binary: {blocked}")

    # Check for WiX File tags
    try:
        tree = ET.fromstring(text)
        for elem in tree.iter():
            for attr_val in elem.attrib.values():
                val_lower = attr_val.lower()
                for blocked in WSA_BLOCKED_NAMES:
                    if blocked in val_lower:
                        raise ValueError(f"Manifest {manifest_path} references forbidden WSA binary in attribute: {attr_val}")
    except ET.ParseError:
        pass


def check_payload_directory(stage_dir: Path):
    if not stage_dir.exists():
        return

    problems = []
    for root, _, files in os.walk(stage_dir):
        for f in files:
            f_lower = f.lower()
            if f_lower in WSA_BLOCKED_NAMES:
                problems.append(f"Forbidden WSA file name found: {f}")
                continue
            for pat in WSA_FORBIDDEN_PATTERNS:
                if pat.match(f):
                    problems.append(f"File matches forbidden WSA pattern: {f}")
                    break

            # Inspect executable binaries for embedded WSA string markers
            if f_lower.endswith((".exe", ".dll")):
                path = Path(root) / f
                try:
                    content = path.read_bytes()
                    for marker in WSA_STRING_MARKERS:
                        if marker in content:
                            problems.append(f"Binary {f} contains forbidden WSA marker '{marker.decode('latin1')}'")
                            break
                except Exception:
                    pass

    if problems:
        raise ValueError("\n".join(problems))


def run_checks(manifests, image_inputs: Path | None, stage_dir: Path | None):
    if image_inputs:
        check_image_inputs(image_inputs)
    for m in manifests:
        check_manifest(Path(m))
    if stage_dir:
        check_payload_directory(Path(stage_dir))


def self_test():
    # Test rejection of blocked file name
    import tempfile
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        (tmp_path / "WsaClient.exe").write_bytes(b"dummy")
        try:
            check_payload_directory(tmp_path)
            raise AssertionError("check_payload_directory failed to reject WsaClient.exe")
        except ValueError:
            pass

        # Test rejection of embedded string
        (tmp_path / "WsaClient.exe").unlink()
        (tmp_path / "custom.dll").write_bytes(b"This is Windows Subsystem for Android payload")
        try:
            check_payload_directory(tmp_path)
            raise AssertionError("check_payload_directory failed to reject embedded WSA marker")
        except ValueError:
            pass

    print("Self test passed.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifests", nargs="*", default=[])
    parser.add_argument("--image-inputs", type=Path, default=None)
    parser.add_argument("--stage-dir", type=Path, default=None)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        self_test()
        return

    try:
        run_checks(args.manifests, args.image_inputs, args.stage_dir)
        print("No Microsoft WSA binaries or components detected in payload or image inputs.")
    except Exception as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
