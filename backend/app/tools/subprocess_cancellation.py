"""Cancellation-safe helpers for thread-backed subprocess execution."""

from __future__ import annotations

import asyncio
import os
import signal
import subprocess
import threading
import time
from collections.abc import Callable
from contextlib import suppress
from pathlib import Path
from typing import Any


async def run_in_thread_cancellable[T](
    func: Callable[..., T], *args: object, **kwargs: object
) -> T:
    """Run a cooperative sync helper and wait for process cleanup on cancel."""
    cancel_event = threading.Event()
    task = asyncio.create_task(
        asyncio.to_thread(func, *args, cancel_event=cancel_event, **kwargs)
    )
    try:
        return await asyncio.shield(task)
    except asyncio.CancelledError:
        cancel_event.set()
        with suppress(asyncio.CancelledError, Exception):
            await asyncio.shield(task)
        raise


def communicate_cancellable(
    argv: list[str],
    timeout: float,
    *,
    cwd: Path | None,
    env: dict[str, str] | None,
    cancel_event: threading.Event,
) -> tuple[bytes, bytes, int]:
    """Run a subprocess, killing and reaping it on timeout or cancellation."""
    if cancel_event.is_set():
        raise asyncio.CancelledError
    proc = subprocess.Popen(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=str(cwd) if cwd is not None else None,
        env=env,
        **process_group_kwargs(),
    )
    deadline = time.monotonic() + timeout
    while True:
        if cancel_event.is_set():
            terminate_process_tree_sync(proc)
            raise asyncio.CancelledError
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            terminate_process_tree_sync(proc)
            raise subprocess.TimeoutExpired(argv, timeout)
        try:
            stdout, stderr = proc.communicate(timeout=min(remaining, 0.05))
            return (
                stdout or b"",
                stderr or b"",
                proc.returncode if proc.returncode is not None else 0,
            )
        except subprocess.TimeoutExpired:
            continue


async def kill_and_reap(proc: asyncio.subprocess.Process) -> None:
    """Terminate an asyncio subprocess tree and always reap its exit status."""
    await asyncio.to_thread(terminate_process_tree_by_pid, proc.pid)
    if proc.returncode is None:
        with suppress(ProcessLookupError):
            proc.kill()
    try:
        await asyncio.wait_for(proc.communicate(), timeout=5)
    except (BrokenPipeError, ConnectionResetError, TimeoutError):
        if proc.returncode is None:
            with suppress(ProcessLookupError):
                proc.kill()
        await proc.wait()


def process_group_kwargs() -> dict[str, Any]:
    """Creation flags that isolate descendants into a killable process group."""
    if os.name == "nt":
        return {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP}
    return {"start_new_session": True}


def terminate_process_tree_by_pid(pid: int) -> None:
    """Force-terminate a process tree without requiring an elevated shell command."""
    if os.name == "nt":
        descendants = _windows_descendant_pids(pid)
        _windows_terminate_pid(pid)
        for child_pid in reversed(descendants):
            _windows_terminate_pid(child_pid)
        # Catch descendants created between the snapshot and parent kill.
        for child_pid in reversed(_windows_descendant_pids(pid)):
            _windows_terminate_pid(child_pid)
        return
    try:
        os.killpg(pid, signal.SIGKILL)  # type: ignore[attr-defined]
    except OSError:
        return


def _windows_descendant_pids(root_pid: int) -> list[int]:
    """Enumerate Windows descendants with Toolhelp32 (no taskkill privileges)."""
    import ctypes
    from ctypes import wintypes

    class ProcessEntry32W(ctypes.Structure):
        _fields_ = [
            ("dwSize", wintypes.DWORD),
            ("cntUsage", wintypes.DWORD),
            ("th32ProcessID", wintypes.DWORD),
            ("th32DefaultHeapID", ctypes.c_size_t),
            ("th32ModuleID", wintypes.DWORD),
            ("cntThreads", wintypes.DWORD),
            ("th32ParentProcessID", wintypes.DWORD),
            ("pcPriClassBase", wintypes.LONG),
            ("dwFlags", wintypes.DWORD),
            ("szExeFile", wintypes.WCHAR * 260),
        ]

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel32.Process32FirstW.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(ProcessEntry32W),
    ]
    kernel32.Process32FirstW.restype = wintypes.BOOL
    kernel32.Process32NextW.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(ProcessEntry32W),
    ]
    kernel32.Process32NextW.restype = wintypes.BOOL
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    snapshot = kernel32.CreateToolhelp32Snapshot(0x00000002, 0)
    if snapshot in (None, 0, ctypes.c_void_p(-1).value):
        return []
    entry = ProcessEntry32W()
    entry.dwSize = ctypes.sizeof(entry)
    parents: dict[int, int] = {}
    try:
        ok = kernel32.Process32FirstW(snapshot, ctypes.byref(entry))
        while ok:
            parents[int(entry.th32ProcessID)] = int(entry.th32ParentProcessID)
            ok = kernel32.Process32NextW(snapshot, ctypes.byref(entry))
    finally:
        kernel32.CloseHandle(snapshot)

    descendants: list[int] = []
    frontier = {root_pid}
    while frontier:
        children = {pid for pid, parent in parents.items() if parent in frontier}
        children.difference_update(descendants)
        if not children:
            break
        descendants.extend(sorted(children))
        frontier = children
    return descendants


def _windows_terminate_pid(pid: int) -> None:
    """Best-effort forced termination of one same-user Windows process."""
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
    kernel32.TerminateProcess.restype = wintypes.BOOL
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    handle = kernel32.OpenProcess(0x0001, False, pid)  # PROCESS_TERMINATE
    if not handle:
        return
    try:
        kernel32.TerminateProcess(handle, 1)
    finally:
        kernel32.CloseHandle(handle)


def terminate_process_tree_sync(proc: subprocess.Popen[bytes]) -> None:
    """Kill a Popen process group and drain its pipes without waiting on descendants."""
    terminate_process_tree_by_pid(proc.pid)
    if proc.poll() is None:
        with suppress(ProcessLookupError):
            proc.kill()
    try:
        proc.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        if proc.stdout is not None:
            proc.stdout.close()
        if proc.stderr is not None:
            proc.stderr.close()
        with suppress(ProcessLookupError):
            proc.kill()
        with suppress(subprocess.TimeoutExpired):
            proc.wait(timeout=5)


__all__ = [
    "communicate_cancellable",
    "kill_and_reap",
    "process_group_kwargs",
    "run_in_thread_cancellable",
]
