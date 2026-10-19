"""Freeze workers for the requested Tauri target, validating every native dependency."""
from pathlib import Path
import argparse
import json
import os
import platform
import shutil
import subprocess
import sysconfig
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RUNTIME = ROOT / "agent-runtime"
MAC_TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin")
TARGETS = (*MAC_TARGETS, "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu")


def host_target():
    if platform.system() == "Windows":
        # Windows ARM64 reports the OS architecture even for x64 Python.
        # PyInstaller freezes the interpreter, so use its build architecture.
        machine = {"win-amd64": "x86_64", "win-arm64": "aarch64", "win32": "i686"}.get(sysconfig.get_platform())
        if machine is None:
            raise RuntimeError(f"Unsupported Windows Python platform: {sysconfig.get_platform()}")
        return f"{machine}-pc-windows-msvc"
    machine = {"arm64": "aarch64", "AMD64": "x86_64"}.get(platform.machine(), platform.machine())
    suffix = {"Darwin": "apple-darwin", "Linux": "unknown-linux-gnu"}.get(platform.system())
    if suffix is None:
        raise RuntimeError("Desktop worker packaging supports macOS, Windows and Linux only")
    return f"{machine}-{suffix}"


def validate_native_files(folder, target):
    executable = folder / ("ew-agent-runtime.exe" if "windows" in target else "ew-agent-runtime")
    if not executable.is_file():
        raise RuntimeError(f"Missing worker: {executable}")
    if target in MAC_TARGETS:
        expected = "arm64" if target.startswith("aarch64") else "x86_64"
        for path in folder.rglob("*"):
            if not path.is_file() or path.is_symlink():
                continue
            with path.open("rb") as source:
                magic = source.read(4)
            if magic not in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca"):
                continue
            archs = subprocess.check_output(["lipo", "-archs", str(path)], text=True).split()
            if expected not in archs:
                raise RuntimeError(f"Wrong architecture ({archs}): {path}; expected {expected}")
    elif "linux" in target:
        for path in folder.rglob("*"):
            if not path.is_file() or path.is_symlink():
                continue
            with path.open("rb") as source:
                header = source.read(20)
            if header[:4] == b"\x7fELF" and (header[4:6] != b"\x02\x01" or header[18:20] != b"\x3e\x00"):
                raise RuntimeError(f"Non-x86_64 Linux binary: {path}")
        if executable.read_bytes()[:4] != b"\x7fELF":
            raise RuntimeError(f"Invalid ELF worker: {executable}")
    else:
        for path in folder.rglob("*"):
            if not path.is_file() or path.suffix.lower() not in (".exe", ".dll", ".pyd"):
                continue
            with path.open("rb") as source:
                if source.read(2) != b"MZ":
                    raise RuntimeError(f"Invalid PE file: {path}")
                source.seek(0x3C)
                offset = int.from_bytes(source.read(4), "little")
                source.seek(offset)
                if source.read(4) != b"PE\0\0" or source.read(2) != b"\x64\x86":
                    raise RuntimeError(f"Non-x86_64 Windows binary: {path}")


def build(target, staging):
    host = host_target()
    if target != host and not (host == MAC_TARGETS[0] and target == MAC_TARGETS[1]):
        raise RuntimeError(f"Cannot build {target} on {host}; use a matching build machine")
    prefix = ["arch", "-x86_64"] if target != host else []
    if prefix:
        subprocess.run([*prefix, "/usr/bin/true"], check=True)  # Rosetta must be present.
    arch = target.split("-")[0]
    system = "macos" if target in MAC_TARGETS else "windows" if "windows" in target else "linux"
    libc = "gnu" if system == "linux" else "none"
    request = f"cpython-3.12.14-{system}-{arch}-{libc}"
    env = dict(os.environ, UV_PROJECT_ENVIRONMENT=str(RUNTIME / f".venv-build-{target}"))
    if target in MAC_TARGETS:
        env["MACOSX_DEPLOYMENT_TARGET"] = "11.0"
    subprocess.run(["uv", "sync", "--locked", "--python", request, "--python-platform", target], cwd=RUNTIME, env=env, check=True)
    python = Path(env["UV_PROJECT_ENVIRONMENT"]) / ("Scripts/python.exe" if "windows" in target else "bin/python")
    run = [*prefix, str(python)]
    subprocess.run([*run, "export_contracts.py", "--check"], cwd=RUNTIME, check=True)
    with tempfile.TemporaryDirectory(prefix="ew-worker-build-") as work:
        # Use the maintained spec, including tokenizer data and dynamic imports.
        # The selected interpreter (and Rosetta prefix above) determines architecture.
        args = [*run, "-m", "PyInstaller", "--noconfirm", "--clean",
                "--distpath", str(Path(work) / "dist"), "--workpath", str(Path(work) / "build"),
                str(RUNTIME / "ew-agent-runtime.spec")]
        subprocess.run(args, cwd=RUNTIME, check=True)
        package = Path(work) / "dist/ew-agent-runtime"
        validate_native_files(package, target)
        shutil.copytree(package, staging / target, symlinks=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default=os.getenv("TAURI_ENV_TARGET_TRIPLE") or os.getenv("CARGO_BUILD_TARGET") or host_target(),
                        choices=(*TARGETS, "universal-apple-darwin"))
    args = parser.parse_args()
    targets = MAC_TARGETS if args.target == "universal-apple-darwin" else (args.target,)
    parent = ROOT / "src-tauri/binaries"
    parent.mkdir(parents=True, exist_ok=True)
    # The previous complete staging stays intact if either architecture fails.
    with tempfile.TemporaryDirectory(prefix=".agent-staging-", dir=parent) as temporary:
        staging = Path(temporary) / "agent"
        staging.mkdir()
        for target in targets:
            build(target, staging)
        (staging / "manifest.json").write_text(json.dumps({"target": args.target, "workers": targets}, indent=2))
        (staging / ".gitkeep").touch()
        destination = parent / "agent"
        previous = parent / ".agent-previous"
        if previous.exists():
            raise RuntimeError(f"Previous staging recovery required: {previous}")
        if destination.exists():
            destination.rename(previous)
        try:
            staging.rename(destination)
        except OSError:
            if previous.exists():
                previous.rename(destination)
            raise
        if previous.exists():
            shutil.rmtree(previous)
    print(f"Validated Agent workers for {args.target}: {', '.join(targets)}")


if __name__ == "__main__":
    main()
