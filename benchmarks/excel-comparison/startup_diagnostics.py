"""Capture only startup-log bytes written during one benchmark session."""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path


MAX_LOG_BYTES = 64 * 1024


@dataclass(frozen=True)
class StartupLogSnapshot:
    path: Path | None
    size: int = 0
    identity: tuple[int, int] | None = None
    error: str | None = None


def snapshot_startup_log() -> StartupLogSnapshot:
    """Record the existing log boundary before starting or loading Excel."""
    local = os.environ.get("LOCALAPPDATA")
    if not local:
        return StartupLogSnapshot(None, error="LOCALAPPDATA is unavailable")
    path = Path(local) / "xlfn-excel-comparison" / "logs" / "startup.log"
    try:
        stat = path.stat()
    except FileNotFoundError:
        return StartupLogSnapshot(path)
    except OSError as error:
        return StartupLogSnapshot(path, error=str(error))
    return StartupLogSnapshot(path, stat.st_size, (stat.st_dev, stat.st_ino))


def read_startup_log_delta(baseline: StartupLogSnapshot) -> dict:
    """Read a bounded tail of new bytes without including previous sessions."""
    result = {"path": str(baseline.path) if baseline.path is not None else None}
    if baseline.error is not None:
        return result | {"status": "unavailable", "error": baseline.error}
    if baseline.path is None:
        return result | {"status": "unavailable", "error": "startup log path is unavailable"}
    try:
        with baseline.path.open("rb") as stream:
            stat = os.fstat(stream.fileno())
            identity = (stat.st_dev, stat.st_ino)
            reset = baseline.identity is not None and (
                identity != baseline.identity or stat.st_size < baseline.size
            )
            start = 0 if reset else baseline.size
            added = max(0, stat.st_size - start)
            skipped = max(0, added - MAX_LOG_BYTES)
            stream.seek(start + skipped)
            # Bound this read to the captured file size, even if a writer appends.
            data = stream.read(min(added, MAX_LOG_BYTES))
    except FileNotFoundError:
        return result | {"status": "missing", "text": "", "bytes_added": 0}
    except OSError as error:
        return result | {"status": "error", "error": str(error)}
    return result | {
        "status": "ok",
        "text": data.decode("utf-8", errors="replace"),
        "bytes_added": added,
        "skipped_bytes": skipped,
        "file_reset": reset,
    }
