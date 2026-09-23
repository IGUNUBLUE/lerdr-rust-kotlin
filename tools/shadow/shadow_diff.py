#!/usr/bin/env python3
"""Phase-3 shadow-diff driver — Go relay and Rust relay, side by side.

Layout under --run-dir (default: a fresh tmp dir):

    run/
      home/                    shared $HOME for both relays (deterministic
                               list_directories output)
      herdr.sock               lerdr-fake-herdr socket (both relays attach)
      herdr-ops.jsonl          fake's method log
      go/   rust-a/  rust-b/   per-side runtime dirs (config/data/auth)
      logs/                    relay + fake stdout/stderr
      traces/                  lerdr-shadow JSONL traces
      report.txt               the normalized diff

Modes:

    self   rust-a vs rust-b — the harness's own determinism gate
    go     go vs rust       — the actual parity gate

Exit: 0 identical, 1 diff, 2 setup/infra failure.
"""

from __future__ import annotations

import argparse
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RELAY_DIR = ROOT / "relay"
ORACLE_DIR = Path(os.environ.get("LERDR_ORACLE", os.path.expanduser("~/Projects/lerdr")))
TOOLS = ROOT / "tools" / "shadow"
SCENARIOS = TOOLS / "scenarios"
DEFAULT_SCENARIO = SCENARIOS / "core.json"
STATE = TOOLS / "herdr" / "state.json"

# Deterministic relay key — exactly 32 bytes; doubles as the bootstrap
# invitation secret the client authenticates with.
TOKEN = "shadow-diff-token-0000-000000000"  # exactly 32 bytes
assert len(TOKEN.encode()) == 32

HANDSHAKE_TIMEOUT_MS = "10000"


def log(msg: str) -> None:
    print(f"[shadow] {msg}", flush=True)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_healthz(port: int, proc: subprocess.Popen, timeout_s: float = 15.0) -> bool:
    url = f"http://127.0.0.1:{port}/healthz"
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return False
        try:
            with urllib.request.urlopen(url, timeout=0.5) as r:
                if r.status == 200:
                    return True
        except OSError:
            pass
        time.sleep(0.05)
    return False


def wait_socket(path: Path, proc: subprocess.Popen, timeout_s: float = 10.0) -> bool:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return False
        if path.exists():
            try:
                conn = socket.socket(socket.AF_UNIX)
                conn.connect(str(path))
                conn.close()
                return True
            except OSError:
                pass
        time.sleep(0.05)
    return False


class Harness:
    def __init__(self, run_dir: Path, keep: bool):
        self.run_dir = run_dir
        self.keep = keep
        self.procs: list[tuple[str, subprocess.Popen]] = []
        self.logs = run_dir / "logs"
        self.traces = run_dir / "traces"
        self.home = run_dir / "home"
        self.sock = run_dir / "herdr.sock"
        self.logs.mkdir(parents=True, exist_ok=True)
        self.traces.mkdir(parents=True, exist_ok=True)
        # A few directories so list_directories has content.
        for d in ("project", "notes", "work"):
            (self.home / d).mkdir(parents=True, exist_ok=True)

    # -- processes -----------------------------------------------------------

    def spawn(self, name: str, argv: list[str], env: dict) -> subprocess.Popen:
        log_file = open(self.logs / f"{name}.log", "w")
        proc = subprocess.Popen(
            argv,
            env=env,
            stdout=log_file,
            stderr=subprocess.STDOUT,
            start_new_session=True,  # own process group → reliable kill
        )
        self.procs.append((name, proc))
        return proc

    def stop_all(self) -> None:
        for name, proc in self.procs:
            if proc.poll() is None:
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
        deadline = time.monotonic() + 5
        for _, proc in self.procs:
            try:
                proc.wait(timeout=max(0.1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass

    # -- env ------------------------------------------------------------------

    def base_env(self, side_dir: Path) -> dict:
        env = dict(os.environ)
        env.update(
            {
                # Shared HOME → identical list_directories output.
                "HOME": str(self.home),
                # Per-side XDG → isolated device-auth stores.
                "XDG_CONFIG_HOME": str(side_dir / "config"),
                "XDG_DATA_HOME": str(side_dir / "data"),
                "XDG_CACHE_HOME": str(side_dir / "cache"),
                "HERDR_SOCKET_PATH": str(self.sock),
                "LERDR_RELAY_TOKEN": TOKEN,
                "HERDR_RELAY_TOKEN": TOKEN,
                "LERDR_RELAY_REARM_BOOTSTRAP": "1",
                "HERDR_RELAY_REARM_BOOTSTRAP": "1",
                # Deterministic logging.
                "LERDR_RELAY_LOG_LEVEL": os.environ.get(
                    "SHADOW_RELAY_LOG", "info"
                ),
                "LERDR_RELAY_LOG_FORMAT": "text",
                "TZ": "UTC",
            }
        )
        # The oracle CLI fake is HERDR_BIN for the Go relay; harmless for Rust.
        env["HERDR_BIN"] = str(self.fake_cli)
        env["FAKE_HERDR_SCENARIO"] = str(STATE)
        env["FAKE_HERDR_STATE"] = str(side_dir / "fake-herdr.state")
        env["FAKE_HERDR_OPERATIONS"] = str(self.logs / f"fake-ops-{side_dir.name}.jsonl")
        return env

    # -- stages ---------------------------------------------------------------

    def build(self, need_go: bool, skip_build: bool = False) -> None:
        if not skip_build:
            log("building rust relay + shadow tools")
            subprocess.run(
                [
                    "cargo",
                    "build",
                    "--bin",
                    "lerdr-relay",
                    "--bin",
                    "lerdr-shadow",
                    "--bin",
                    "lerdr-fake-herdr",
                ],
                cwd=RELAY_DIR,
                check=True,
            )
        self.rust_relay = RELAY_DIR / "target" / "debug" / "lerdr-relay"
        self.shadow = RELAY_DIR / "target" / "debug" / "lerdr-shadow"
        self.fake_socket = RELAY_DIR / "target" / "debug" / "lerdr-fake-herdr"
        for b in (self.rust_relay, self.shadow, self.fake_socket):
            if not b.exists():
                raise SystemExit(f"missing binary: {b}")
        if need_go:
            bindir = self.run_dir / "bin"
            bindir.mkdir(exist_ok=True)
            self.go_relay = bindir / "lerdr-go"
            self.fake_cli = bindir / "fake-herdr"
            if skip_build and self.go_relay.exists() and self.fake_cli.exists():
                return
            log("building oracle binaries (no oracle source changes)")
            subprocess.run(
                ["go", "build", "-o", str(self.go_relay), "./cmd/lerdr"],
                cwd=ORACLE_DIR,
                check=True,
            )
            subprocess.run(
                ["go", "build", "-o", str(self.fake_cli), "./cmd/fake-herdr"],
                cwd=ORACLE_DIR,
                check=True,
            )
        else:
            # Self-parity still wants the CLI fake around for symmetry, but
            # nothing calls it — point at /bin/true.
            self.fake_cli = Path("/bin/true")

    def start_fake(self) -> None:
        proc = self.spawn(
            "fake-herdr-socket",
            [
                str(self.fake_socket),
                "--socket",
                str(self.sock),
                "--state",
                str(STATE),
                "--ops-log",
                str(self.run_dir / "herdr-ops.jsonl"),
            ],
            dict(os.environ),
        )
        if not wait_socket(self.sock, proc):
            raise SystemExit("fake herdr socket never came up — see logs/fake-herdr-socket.log")
        log("fake herdr socket up")

    def start_relay(self, side: str, kind: str, port: int, plugin_port: int) -> None:
        side_dir = self.run_dir / side
        side_dir.mkdir(parents=True, exist_ok=True)
        env = self.base_env(side_dir)
        if kind == "go":
            env.update(
                {
                    "LERDR_RELAY_PORT": str(port),
                    "LERDR_RELAY_PLUGIN_PORT": str(plugin_port),
                    "LERDR_RELAY_HOST": "127.0.0.1",
                    "LERDR_RELAY_INSTANCE_ID": f"shadow-{side}",
                    # Long interval → no mid-scenario poll; startup state stays
                    # authoritative (invalidations still flow via the socket).
                    "LERDR_RELAY_POLL_INTERVAL": "60",
                }
            )
            argv = [str(self.go_relay), "serve"]
        else:
            argv = [
                str(self.rust_relay),
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                str(port),
                "--token",
                TOKEN,
                "--socket-path",
                str(self.sock),
                "--runtime-dir",
                str(side_dir / "runtime"),
                "--device-auth-dir",
                str(side_dir / "device-auth"),
                "--rearm-bootstrap",
            ]
        proc = self.spawn(f"relay-{side}", argv, env)
        if not wait_healthz(port, proc):
            raise SystemExit(
                f"relay {side} never answered /healthz — see logs/relay-{side}.log"
            )
        log(f"relay {side} ({kind}) up on :{port}")

    def run_client(self, side: str, port: int, scenario: Path) -> Path:
        trace = self.traces / f"{side}.jsonl"
        url = f"ws://127.0.0.1:{port}/ws"
        cmd = [
            str(self.shadow),
            "run",
            "--url",
            url,
            "--token",
            TOKEN,
            "--scenario",
            str(scenario),
            "--trace",
            str(trace),
            "--side",
            side,
            "--handshake-timeout-ms",
            HANDSHAKE_TIMEOUT_MS,
            # `fake_call` steps (control.set/control.emit) drive server-side
            # change through the same socket the relays attach to.
            "--herdr-socket",
            str(self.sock),
        ]
        log(f"running scenario against {side} …")
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=180)
        if proc.returncode != 0:
            raise SystemExit(
                f"shadow client failed on {side} (exit {proc.returncode}):\n"
                f"{proc.stdout}\n{proc.stderr}\npartial trace: {trace}"
            )
        frames = sum(1 for line in trace.read_text().splitlines() if '"kind":"rx"' in line)
        log(f"{side}: {frames} rx frames traced")
        return trace

    def diff(self, a: Path, b: Path) -> int:
        report = self.run_dir / "report.txt"
        proc = subprocess.run(
            [str(self.shadow), "diff", "--a", str(a), "--b", str(b), "--out", str(report)],
            capture_output=True,
            text=True,
        )
        sys.stdout.write(report.read_text() if report.exists() else proc.stdout)
        if proc.stderr:
            sys.stderr.write(proc.stderr)
        return proc.returncode


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--mode", choices=["self", "go"], default="go")
    ap.add_argument(
        "--scenario",
        type=Path,
        default=DEFAULT_SCENARIO,
        help="scenario JSON — a name under tools/shadow/scenarios/ or a path "
        "(default: core.json)",
    )
    ap.add_argument("--run-dir", type=Path, default=None, help="reuse/keep a run dir")
    ap.add_argument("--keep", action="store_true", help="keep the run dir (default when --run-dir given)")
    ap.add_argument("--skip-build", action="store_true", help="binaries already built")
    args = ap.parse_args()

    scenario = args.scenario
    if not scenario.exists():
        # Bare names resolve under scenarios/ ("watch" → watch.json).
        candidate = SCENARIOS / scenario.name
        if candidate.suffix != ".json":
            candidate = candidate.with_suffix(".json")
        if candidate.exists():
            scenario = candidate
    if not scenario.exists():
        raise SystemExit(f"scenario not found: {args.scenario}")
    scenario = scenario.resolve()
    log(f"scenario: {scenario}")

    run_dir = args.run_dir or Path(tempfile.mkdtemp(prefix="lerdr-shadow-"))
    run_dir.mkdir(parents=True, exist_ok=True)
    keep = args.keep or args.run_dir is not None
    log(f"run dir: {run_dir}")

    h = Harness(run_dir, keep)
    try:
        h.build(need_go=args.mode == "go", skip_build=args.skip_build)
        h.start_fake()

        if args.mode == "go":
            sides = [("go", "go"), ("rust", "rust")]
        else:
            sides = [("rust-a", "rust"), ("rust-b", "rust")]

        ports = {side: free_port() for side, _ in sides}
        plugin_ports = {side: free_port() for side, _ in sides}
        for side, kind in sides:
            h.start_relay(side, kind, ports[side], plugin_ports[side])

        traces = {side: h.run_client(side, ports[side], scenario) for side, _ in sides}
        rc = h.diff(traces[sides[0][0]], traces[sides[1][0]])
        return rc
    finally:
        h.stop_all()
        if keep:
            log(f"kept run dir: {run_dir}")
        else:
            import shutil

            shutil.rmtree(run_dir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
