"""Shared, deterministic workload definitions for the two XLLs.

The smoke profile is for checking the Windows harness, not for comparing speed.
"""

from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class Case:
    id: str
    variant: str
    params: dict[str, Any]
    timeout_s: int = 600

    @property
    def key(self) -> str:
        return f"{self.id}/{self.variant}"


def cases(profile: str = "full") -> list[Case]:
    if profile not in ("full", "smoke"):
        raise ValueError(profile)
    smoke = profile == "smoke"
    result: list[Case] = []

    def add(id: str, variant: str, timeout_s: int = 600, **params: Any) -> None:
        result.append(Case(id, variant, params, timeout_s))

    for n in ([100] if smoke else [1_000, 10_000, 100_000]):
        add("S01", str(n), cells=n)
    for argc in (2, 4, 8):
        add("S02", str(argc), cells=100 if smoke else 10_000, argc=argc)
    for us in (0, 1, 10, 100, 1_000):
        add("S03", f"{us}us", cells=100 if smoke else 1_000, delay_us=us)
    for period in (0, 2, 10, 100):
        add("S04", f"period-{period}", cells=100 if smoke else 10_000, period=period)

    dimensions = [(10, 10)] if smoke else [(10, 10), (100, 100), (1_000, 100)]
    for id in ("M01", "M02", "M03"):
        for rows, cols in dimensions:
            add(id, f"{rows}x{cols}", rows=rows, cols=cols)
    for rows, cols in ([(10, 10)] if smoke else [(1_000, 10), (100, 100), (10, 1_000)]):
        add("M04", f"{rows}x{cols}", rows=rows, cols=cols)
    for rows, cols in dimensions:
        add("M05", f"{rows}x{cols}", rows=rows, cols=cols)

    for id in ("T01", "T02", "T03"):
        for kind in ("ascii", "ja"):
            for length in ([8] if smoke else [8, 64, 512, 4_096]):
                add(id, f"{kind}-{length}", length=length, kind=kind, cells=20 if smoke else 1_000)

    for id, delay in (("P01", 100), ("P02", 0), ("P03", 100), ("P03", 1_000), ("P04", 0)):
        for threads in ([1, 2] if smoke else [1, 2, 4, 8, 16]):
            variant = f"{delay}us-{threads}" if id == "P03" else str(threads)
            add(id, variant, threads=threads, delay_us=delay,
                cells=100 if smoke else (10_000 if delay == 0 else 1_000))

    add("A01", "immediate", cells=20 if smoke else 1_000, delay_us=0)
    for us in (100, 1_000, 10_000, 100_000):
        add("A02", f"{us}us", cells=20 if smoke else 1_000, delay_us=us)
    for n in ([20] if smoke else [100, 1_000, 10_000]):
        add("A03", str(n), cells=n, delay_us=-1)
    add("A04", "burst", cells=20 if smoke else 10_000, delay_us=-1)
    for mode in ("recalc", "clear", "close"):
        add("A05", mode, cells=20 if smoke else 1_000, delay_us=1_000_000, mode=mode)
    add("A06", "repeated", cells=20 if smoke else 1_000, delay_us=10_000,
        repetitions=3 if smoke else 100)

    for n in ([20] if smoke else [100, 1_000, 10_000, 100_000]):
        add("R01", str(n), cells=n, topics=n, timeout_s=1_200)
    add("R02", "shared", cells=20 if smoke else 10_000, topics=1)
    for n, topics in ([(20, 4)] if smoke else [(10_000, 10), (10_000, 100), (100_000, 1_000)]):
        add("R03", f"{n}-{topics}", cells=n, topics=topics, timeout_s=1_200)
    for hz in (1, 10, 100, 1_000, 2_000):
        add("R04", f"{hz}hz", cells=20 if smoke else 100, topics=10,
            period_ms=1_000 / hz, requested_hz=hz, duration_s=2 if smoke else 10)
    add("R05", "burst", cells=20 if smoke else 10_000, topics=20 if smoke else 10_000)
    add("R06", "churn", cells=20 if smoke else 10_000, topics=20 if smoke else 10_000,
        repetitions=2 if smoke else 20)
    add("R07", "long", cells=20 if smoke else 1_000, topics=20 if smoke else 100,
        period_ms=100, duration_s=3 if smoke else 1_800, timeout_s=2_400)

    add("C01", "cold")
    for n in ([10] if smoke else [10, 100, 1_000, 5_000]):
        add("C02", str(n), registrations=n, timeout_s=1_200)
    add("C03", "first")
    add("C04", "warm-open", cells=100 if smoke else 10_000)

    add("W01", "mixed", cells=100 if smoke else 10_000)
    add("W02", "finance", cells=100 if smoke else 10_000)
    for n in ([100] if smoke else [10_000, 100_000]):
        add("W03", str(n), cells=n)
    for workload in ("scalar", "matrix", "async", "rtd"):
        add("L01", workload, workload=workload, cells=(100 if smoke else 10_000))
    for n in ([3] if smoke else [100, 1_000]):
        add("L02", str(n), cells=100 if smoke else 1_000, repetitions=n)
    for workload in ("scalar", "async", "rtd"):
        add("L03", workload, workload=workload, cells=20 if smoke else 1_000, repetitions=3 if smoke else 100)
    return result


IDS = tuple(dict.fromkeys(case.id for case in cases("full")))
