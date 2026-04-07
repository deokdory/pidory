#!/usr/bin/env python3
"""
pidory menubar — small status indicator for the pidory git repo + service.

Lives in the macOS menu bar and shows one of these states:
  ✓  synced       — local == remote, binary fresh, service fresh
  ⬇  pull needed  — remote ahead of local
  🔨 build needed — HEAD newer than the release binary
  ↻  restart     — binary newer than the running service
  ⏳ working     — an action is in progress
  ⚠  error       — last action failed; click for details

Click the icon for status detail and one-click actions.

Repo path is auto-detected from this file's location, so the script
must live at <repo>/tools/menubar/menubar.py.
"""

from __future__ import annotations

import datetime
import os
import subprocess
import threading
import time
from pathlib import Path

import rumps

# Auto-detect repo: this file is at <REPO>/tools/menubar/menubar.py
REPO = Path(__file__).resolve().parents[2]
BINARY = REPO / "target" / "release" / "pidory"
SERVICE = "com.pidory.bot"
LOG = Path.home() / ".pidory" / "menubar.log"

REFRESH_SECONDS = 300       # local filesystem refresh (5 min)
FETCH_SECONDS = 24 * 3600   # remote git fetch (once per day)


def log(msg: str) -> None:
    LOG.parent.mkdir(parents=True, exist_ok=True)
    stamp = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    with LOG.open("a") as f:
        f.write(f"[{stamp}] {msg}\n")


def run(cmd: list[str], cwd: Path | None = None, timeout: int = 600) -> tuple[int, str, str]:
    """Run a command, return (rc, stdout, stderr)."""
    try:
        p = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return p.returncode, p.stdout.strip(), p.stderr.strip()
    except subprocess.TimeoutExpired:
        return 124, "", f"timeout after {timeout}s"
    except FileNotFoundError as e:
        return 127, "", str(e)


# ---------- state detection ----------

def git_local_sha() -> str | None:
    rc, out, _ = run(["git", "rev-parse", "HEAD"], cwd=REPO)
    return out if rc == 0 else None


def git_remote_sha() -> str | None:
    rc, out, _ = run(["git", "rev-parse", "@{u}"], cwd=REPO)
    return out if rc == 0 else None


def git_head_commit_time() -> int | None:
    rc, out, _ = run(["git", "log", "-1", "--format=%ct", "HEAD"], cwd=REPO)
    if rc != 0 or not out:
        return None
    try:
        return int(out)
    except ValueError:
        return None


def git_head_subject() -> str:
    rc, out, _ = run(["git", "log", "-1", "--format=%h %s", "HEAD"], cwd=REPO)
    return out if rc == 0 else "(unknown)"


def git_branch() -> str:
    rc, out, _ = run(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=REPO)
    return out if rc == 0 else "(unknown)"


def git_commits_behind() -> int:
    rc, out, _ = run(["git", "rev-list", "--count", "HEAD..@{u}"], cwd=REPO)
    if rc != 0 or not out:
        return 0
    try:
        return int(out)
    except ValueError:
        return 0


# Files that we'll auto-reset before pull when they're the only thing dirty.
# These regenerate themselves on the next build, so discarding is safe.
AUTO_RESET_FILES = {"Cargo.lock"}


def dirty_files() -> list[str]:
    """Return list of files with uncommitted modifications (tracked only)."""
    rc, out, _ = run(["git", "diff", "--name-only"], cwd=REPO)
    if rc != 0 or not out:
        return []
    return [line.strip() for line in out.splitlines() if line.strip()]


def binary_mtime() -> float | None:
    try:
        return BINARY.stat().st_mtime
    except FileNotFoundError:
        return None


def service_pid() -> int | None:
    rc, out, _ = run(["launchctl", "list"])
    if rc != 0:
        return None
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[2] == SERVICE:
            try:
                pid = int(parts[0])
                return pid if pid > 0 else None
            except ValueError:
                return None
    return None


def service_start_time(pid: int) -> float | None:
    """Return process start time as a unix timestamp (epoch seconds)."""
    rc, out, _ = run(["ps", "-p", str(pid), "-o", "lstart="])
    if rc != 0 or not out:
        return None
    try:
        # lstart format: "Tue Apr  7 15:23:01 2026"
        return time.mktime(time.strptime(out.strip(), "%a %b %d %H:%M:%S %Y"))
    except ValueError:
        return None


# ---------- state machine ----------

STATE_SYNCED = "synced"
STATE_PULL = "pull"
STATE_BUILD = "build"
STATE_RESTART = "restart"
STATE_WORKING = "working"
STATE_ERROR = "error"

ICONS = {
    STATE_SYNCED: "✓",
    STATE_PULL: "⬇",
    STATE_BUILD: "🔨",
    STATE_RESTART: "↻",
    STATE_WORKING: "⏳",
    STATE_ERROR: "⚠",
}


def detect_state() -> dict:
    """Inspect repo + binary + service and return a state dict."""
    local = git_local_sha()
    remote = git_remote_sha()
    behind = git_commits_behind() if (local and remote) else 0
    binmtime = binary_mtime()
    headtime = git_head_commit_time()
    pid = service_pid()
    svc_start = service_start_time(pid) if pid else None
    dirty = dirty_files()

    state = STATE_SYNCED
    detail = "All up to date"

    if local and remote and local != remote:
        state = STATE_PULL
        detail = f"{behind} commit{'s' if behind != 1 else ''} behind origin"
    elif binmtime is None or (headtime is not None and binmtime < headtime):
        state = STATE_BUILD
        detail = "Binary out of date — rebuild needed"
    elif pid is None:
        state = STATE_RESTART
        detail = "Service not running"
    elif binmtime is not None and svc_start is not None and svc_start < binmtime:
        state = STATE_RESTART
        detail = "Service running an older binary"

    return {
        "dirty": dirty,
        "state": state,
        "detail": detail,
        "branch": git_branch(),
        "head": git_head_subject(),
        "behind": behind,
        "pid": pid,
    }


# ---------- mtime-based cache ----------
#
# detect_state() is the expensive part of a refresh tick (~250ms of fork+exec).
# We watch the three files whose mtimes determine every state transition:
#
#   - .git/HEAD          — changes on commit / branch switch / pull
#   - .git/FETCH_HEAD    — changes on git fetch
#   - target/release/pidory — changes on cargo build --release
#
# If none of these mtimes have moved since the last successful detect_state,
# we return the cached state dict and pay zero subprocess cost. Worst-case
# staleness is bounded by REFRESH_SECONDS anyway, so the only thing the cache
# can mask is "service crashed without binary changing" — which the next tick
# will pick up at most REFRESH_SECONDS later. Acceptable.

WATCHED_PATHS = (
    REPO / ".git" / "HEAD",
    REPO / ".git" / "FETCH_HEAD",
    BINARY,
)


def watched_mtimes() -> tuple[float, ...]:
    out = []
    for p in WATCHED_PATHS:
        try:
            out.append(p.stat().st_mtime)
        except FileNotFoundError:
            out.append(0.0)
    return tuple(out)


class StateCache:
    def __init__(self) -> None:
        self.state: dict | None = None
        self.mtimes: tuple[float, ...] | None = None

    def get(self, force: bool = False) -> dict:
        current = watched_mtimes()
        if not force and self.state is not None and current == self.mtimes:
            return self.state
        self.state = detect_state()
        self.mtimes = current
        return self.state

    def invalidate(self) -> None:
        self.state = None
        self.mtimes = None


# ---------- the app ----------

class PidoryMenuBar(rumps.App):
    def __init__(self) -> None:
        super().__init__("pidory", title=ICONS[STATE_SYNCED] + " pidory", quit_button=None)

        self.busy = False
        self.last_fetch = 0.0
        self.state: dict = {}
        self.last_error: str | None = None
        self.cache = StateCache()

        self.status_item = rumps.MenuItem("Loading…")
        self.head_item = rumps.MenuItem("")
        self.branch_item = rumps.MenuItem("")
        self.pid_item = rumps.MenuItem("")
        self.dirty_item = rumps.MenuItem("")
        self.error_item = rumps.MenuItem("")

        self.pull_item = rumps.MenuItem("Pull", callback=self.on_pull)
        self.build_item = rumps.MenuItem("Build", callback=self.on_build)
        self.restart_item = rumps.MenuItem("Restart service", callback=self.on_restart)
        self.update_all_item = rumps.MenuItem("Update everything", callback=self.on_update_all)
        self.clear_error_item = rumps.MenuItem("Clear error", callback=self.on_clear_error)

        self.menu = [
            self.status_item,
            self.head_item,
            self.branch_item,
            self.pid_item,
            self.dirty_item,
            self.error_item,
            None,
            rumps.MenuItem("Refresh now", callback=self.on_refresh),
            None,
            self.pull_item,
            self.build_item,
            self.restart_item,
            None,
            self.update_all_item,
            self.clear_error_item,
            None,
            rumps.MenuItem("Open log", callback=self.on_open_log),
            rumps.MenuItem("Quit", callback=rumps.quit_application),
        ]

        # Initial fetch + refresh in background so we don't block startup.
        threading.Thread(target=self._initial_refresh, daemon=True).start()

    # ---- refresh ----

    def _initial_refresh(self) -> None:
        self._do_fetch()
        self.refresh(force=True)

    def _do_fetch(self) -> None:
        log("git fetch")
        rc, _, err = run(["git", "fetch", "--quiet"], cwd=REPO, timeout=120)
        if rc != 0:
            log(f"fetch failed: {err}")
        self.last_fetch = time.time()

    def refresh(self, force: bool = False) -> None:
        if self.busy:
            return
        self.state = self.cache.get(force=force)
        self._render()

    def _render(self) -> None:
        s = self.state

        # Icon priority: working > error > underlying state
        if self.busy:
            icon = ICONS[STATE_WORKING]
        elif self.last_error:
            icon = ICONS[STATE_ERROR]
        else:
            icon = ICONS[s["state"]]
        self.title = f"{icon} pidory"

        self.status_item.title = f"Status: {s['detail']}"
        self.head_item.title = f"HEAD: {s['head']}"
        self.branch_item.title = f"Branch: {s['branch']}"
        self.pid_item.title = f"Service PID: {s['pid'] or '(stopped)'}"

        dirty = s.get("dirty", [])
        if dirty:
            shown = ", ".join(dirty[:3])
            if len(dirty) > 3:
                shown += f" (+{len(dirty) - 3})"
            self.dirty_item.title = f"Dirty: {shown}"
        else:
            self.dirty_item.title = ""

        if self.last_error:
            err = self.last_error.replace("\n", " ")
            if len(err) > 120:
                err = err[:117] + "…"
            self.error_item.title = f"⚠ {err}"
        else:
            self.error_item.title = ""

        # Enable/disable action items based on state
        st = s["state"]
        self.pull_item.set_callback(self.on_pull if st == STATE_PULL and not self.busy else None)
        self.build_item.set_callback(self.on_build if st == STATE_BUILD and not self.busy else None)
        self.restart_item.set_callback(self.on_restart if st == STATE_RESTART and not self.busy else None)
        self.update_all_item.set_callback(
            self.on_update_all if st != STATE_SYNCED and not self.busy else None
        )
        self.clear_error_item.set_callback(
            self.on_clear_error if self.last_error and not self.busy else None
        )

    # ---- timers ----

    @rumps.timer(REFRESH_SECONDS)
    def _periodic_refresh(self, _sender) -> None:
        # Daily fetch
        if time.time() - self.last_fetch >= FETCH_SECONDS:
            threading.Thread(target=self._fetch_then_refresh, daemon=True).start()
        else:
            self.refresh()

    def _fetch_then_refresh(self) -> None:
        self._do_fetch()
        # Force re-detect: fetch may have updated FETCH_HEAD even if mtime
        # tick resolution rounds it to the same second as the cached value.
        self.refresh(force=True)

    # ---- actions ----

    def _run_async(self, fn) -> None:
        if self.busy:
            return
        self.busy = True
        self.last_error = None
        self._render()

        def worker() -> None:
            try:
                fn()
            finally:
                self.busy = False
                self.cache.invalidate()
                self.refresh(force=True)

        threading.Thread(target=worker, daemon=True).start()

    def on_refresh(self, _sender) -> None:
        self._run_async(self._fetch_then_refresh)

    def on_pull(self, _sender) -> None:
        self._run_async(self._do_pull)

    def on_build(self, _sender) -> None:
        self._run_async(self._do_build)

    def on_restart(self, _sender) -> None:
        self._run_async(self._do_restart)

    def on_update_all(self, _sender) -> None:
        def chain() -> None:
            cur = self.state.get("state")
            if cur == STATE_PULL:
                if not self._do_pull():
                    return
                self.cache.invalidate()
            self.state = self.cache.get(force=True)
            if self.state["state"] == STATE_BUILD:
                if not self._do_build():
                    return
                self.cache.invalidate()
            self.state = self.cache.get(force=True)
            if self.state["state"] == STATE_RESTART:
                self._do_restart()

        self._run_async(chain)

    # ---- ops ----

    def _set_error(self, msg: str) -> None:
        self.last_error = msg
        log(f"ERROR: {msg}")

    def _auto_reset_safe_dirty(self) -> None:
        """Reset auto-regenerable files (Cargo.lock) if they're dirty."""
        dirty = dirty_files()
        safe = [f for f in dirty if f in AUTO_RESET_FILES]
        if safe:
            log(f"auto-reset: {safe}")
            run(["git", "checkout", "--"] + safe, cwd=REPO)

    def _do_pull(self) -> bool:
        self._auto_reset_safe_dirty()
        log("git pull --ff-only")
        rc, out, err = run(["git", "pull", "--ff-only"], cwd=REPO, timeout=120)
        log(f"pull rc={rc} out={out[:200]} err={err[:200]}")
        if rc != 0:
            self._set_error(f"pull failed: {err or out}")
            return False
        return True

    def _do_build(self) -> bool:
        log("cargo build --release")
        rc, out, err = run(
            ["cargo", "build", "--release"], cwd=REPO, timeout=1800
        )
        log(f"build rc={rc} err={err[-400:]}")
        if rc != 0:
            tail = err.strip().splitlines()[-3:] if err else []
            self._set_error("build failed: " + " | ".join(tail) if tail else "build failed")
            return False
        return True

    def _do_restart(self) -> bool:
        log("launchctl kickstart -k")
        uid = os.getuid()
        rc, out, err = run(
            ["launchctl", "kickstart", "-k", f"gui/{uid}/{SERVICE}"], timeout=30
        )
        log(f"restart rc={rc} err={err}")
        if rc != 0:
            self._set_error(f"restart failed: {err or out}")
            return False
        return True

    def on_clear_error(self, _sender) -> None:
        self.last_error = None
        self.refresh()

    def on_open_log(self, _sender) -> None:
        run(["open", str(LOG)])


if __name__ == "__main__":
    PidoryMenuBar().run()
