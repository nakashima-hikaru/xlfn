"""Run proof-sensitivity checks with invocation-local baselines and isolated workers."""

from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]


def is_verification_failure(returncode: int, output: str) -> bool:
    """Require a failed proof result plus a specific verifier diagnostic."""
    proof_failure = any(marker in output for marker in (
        "precondition not satisfied", "postcondition not satisfied",
        "invariant not satisfied", "assertion failed", "bitvector assertion not satisfied",
        "could not show invariant", "Cannot show invariant holds at end of block",
        "constructed value may fail to meet its declared type invariant",
        "unable to prove assertion safety condition",
    ))
    verified_failure = re.search(r"verification results:: \d+ verified, [1-9]\d* errors", output)
    return returncode != 0 and proof_failure and verified_failure is not None


def is_idle_callback_ownership_rejection(returncode: int, output: str) -> bool:
    """The reordered callback must move its linear handoff before borrowing it."""
    return returncode != 0 and "error[E0382]: borrow of moved value: `detached`" in output


@dataclass(frozen=True)
class Snapshot:
    proof: Path
    files: tuple[tuple[Path, bytes], ...]

    @classmethod
    def read(cls, proof: Path, dependencies: tuple[Path, ...]) -> "Snapshot":
        for path in (proof, *dependencies):
            if path.is_absolute() or ".." in path.parts:
                raise SystemExit(f"FAIL: snapshot path must remain relative: {path}")
        files = {}
        # Match copytree's original behavior for linked files and directories.
        for directory, _directories, names in os.walk(ROOT / proof, followlinks=True):
            for name in names:
                path = Path(directory) / name
                files[path.relative_to(ROOT)] = path.read_bytes()
        for path in dependencies:
            files[path] = (ROOT / path).read_bytes()
        return cls(proof, tuple(sorted(files.items())))

    @property
    def digest(self) -> str:
        digest = hashlib.sha256()
        for path, data in self.files:
            for part in (path.as_posix().encode(), data):
                digest.update(len(part).to_bytes(8, "big"))
                digest.update(part)
        return digest.hexdigest()

    @property
    def key(self) -> tuple[Path, str]:
        return self.proof, self.digest

    def source(self, path: Path) -> str:
        # Match Path.read_text's universal-newline behavior in the original gate.
        return dict(self.files)[path].decode().replace("\r\n", "\n").replace("\r", "\n")

    def write(self, tree: Path) -> None:
        for path, data in self.files:
            target = tree / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)


@dataclass(frozen=True)
class Mutation:
    snapshot: Snapshot
    protocol: Path
    name: str
    before: str
    after: str
    ownership_rejection: bool = False

    @property
    def label(self) -> str:
        return f"{self.snapshot.proof.name}: {self.protocol}: {self.name}"

    def changed_source(self) -> str:
        source = self.snapshot.source(self.protocol)
        if source.count(self.before) != 1:
            raise SystemExit(f"FAIL: mutation anchor changed: {self.label}")
        if self.before == self.after:
            raise SystemExit(f"FAIL: mutation does not change source: {self.label}")
        return source.replace(self.before, self.after)

    def describe(self) -> dict:
        return {
            "proof": str(self.snapshot.proof), "protocol": str(self.protocol),
            "name": self.name, "baseline_sha256": self.snapshot.digest,
            "changed_source_sha256": hashlib.sha256(self.changed_source().encode()).hexdigest(),
            "rejection": "ownership-E0382-detached" if self.ownership_rejection else "proof",
        }


class MutationPlan:
    def __init__(self) -> None:
        self.snapshots: dict[tuple[Path, str], Snapshot] = {}
        self.mutations: list[Mutation] = []
        self.groups = 0

    def add_baseline(self, snapshot: Snapshot) -> Snapshot:
        return self.snapshots.setdefault(snapshot.key, snapshot)

    def add_group(self, proof: Path, protocol: Path, dependencies: tuple[Path, ...],
                  mutations: dict[str, tuple[str, str]],
                  ownership_rejections: frozenset[str] = frozenset()) -> None:
        unknown = ownership_rejections - mutations.keys()
        if unknown:
            raise SystemExit(f"FAIL: ownership rejection names have no mutation: {sorted(unknown)}")
        snapshot = self.add_baseline(Snapshot.read(proof, (protocol, *dependencies)))
        for name, (before, after) in mutations.items():
            mutation = Mutation(snapshot, protocol, name, before, after, name in ownership_rejections)
            mutation.changed_source()  # Validate every anchor before launching any verifier.
            self.mutations.append(mutation)
        self.groups += 1

    def shard(self, index: int, count: int) -> "MutationPlan":
        if count < 1 or not 0 <= index < count:
            raise ValueError("shard index must be in [0, shard count)")
        plan = MutationPlan()
        # Each runner proves its own exact inputs; no proof results cross machines.
        plan.snapshots = self.snapshots.copy()
        plan.groups = self.groups
        plan.mutations = self.mutations[index::count]
        return plan


_collecting: MutationPlan | None = None


@contextmanager
def collect_mutations(plan: MutationPlan):
    global _collecting
    if _collecting is not None:
        raise RuntimeError("mutation collection is already active")
    _collecting = plan
    try:
        yield
    finally:
        _collecting = None


def check_mutations(proof: Path, protocol: Path, dependencies: tuple[Path, ...],
                    mutations: dict[str, tuple[str, str]],
                    ownership_rejections: frozenset[str] = frozenset()) -> None:
    if _collecting is not None:
        _collecting.add_group(proof, protocol, dependencies, mutations, ownership_rejections)
        return
    # Individual checker entry points remain useful for focused diagnostics.
    plan = MutationPlan()
    plan.add_group(proof, protocol, dependencies, mutations, ownership_rejections)
    MutationRunner(jobs=1).run(plan)


def cpu_count() -> int:
    if hasattr(os, "sched_getaffinity"):
        return len(os.sched_getaffinity(0))
    return (getattr(os, "process_cpu_count", os.cpu_count)() or 1)


class MutationRunner:
    def __init__(self, jobs: int = 2, threads: int | None = None) -> None:
        if jobs < 1 or (threads is not None and threads < 1):
            raise ValueError("jobs and threads must be positive")
        self.jobs = jobs
        self.threads = threads if threads is not None else max(1, cpu_count() // jobs)

    def invoke(self, tree: Path, proof: Path) -> tuple[int, str, float]:
        command = ["verus", "--crate-type=lib", "--num-threads", str(self.threads),
                   str(proof / "src/lib.rs")]
        started = time.monotonic()
        try:
            result = subprocess.run(command, cwd=tree, capture_output=True, text=True, timeout=120)
        except subprocess.TimeoutExpired as error:
            raise SystemExit(f"FAIL: verifier timed out for {proof}") from error
        return result.returncode, result.stdout + result.stderr, time.monotonic() - started

    def baseline(self, snapshot: Snapshot) -> dict:
        with tempfile.TemporaryDirectory(prefix="xlfn-verus-baseline-") as directory:
            tree = Path(directory)
            snapshot.write(tree)
            code, output, elapsed = self.invoke(tree, snapshot.proof)
        summary = re.search(r"verification results:: ([1-9]\d*) verified, 0 errors", output)
        if code != 0 or summary is None:
            raise SystemExit(f"FAIL: unmodified baseline must verify before mutations ({snapshot.proof}):\n{output}")
        print(f"PASS: baseline {snapshot.proof.name}, {summary.group(1)} verified ({elapsed:.2f}s)", flush=True)
        return {"proof": str(snapshot.proof), "sha256": snapshot.digest,
                "verified": int(summary.group(1)), "seconds": elapsed}

    def mutation(self, mutation: Mutation) -> dict:
        with tempfile.TemporaryDirectory(prefix="xlfn-verus-mutation-") as directory:
            tree = Path(directory)
            mutation.snapshot.write(tree)
            (tree / mutation.protocol).write_text(mutation.changed_source())
            code, output, elapsed = self.invoke(tree, mutation.snapshot.proof)
        if mutation.ownership_rejection:
            if not is_idle_callback_ownership_rejection(code, output):
                raise SystemExit(f"FAIL: {mutation.label} was not rejected by ownership checking:\n{output}")
        elif not is_verification_failure(code, output):
            raise SystemExit(f"FAIL: {mutation.label} did not fail verification:\n{output}")
        print(f"PASS: rejected {mutation.label} ({elapsed:.2f}s)", flush=True)
        return {**mutation.describe(), "seconds": elapsed}

    def run(self, plan: MutationPlan) -> dict:
        started = time.monotonic()
        # Validate plans supplied directly as well as plans assembled by add_group.
        for mutation in plan.mutations:
            mutation.changed_source()
        print(f"Verus: {len(plan.snapshots)} baselines, {len(plan.mutations)} mutations, "
              f"{self.jobs} workers x {self.threads} verifier threads", flush=True)
        # No mutation can count as a pass until every exact source snapshot verifies.
        baselines = [self.baseline(snapshot) for snapshot in plan.snapshots.values()]
        results: dict[Mutation, dict] = {}
        with ThreadPoolExecutor(max_workers=self.jobs) as executor:
            pending = {executor.submit(self.mutation, mutation): mutation for mutation in plan.mutations}
            try:
                for future in as_completed(pending):
                    results[pending[future]] = future.result()
            except BaseException:
                for future in pending:
                    future.cancel()
                raise
        elapsed = time.monotonic() - started
        print(f"PASS: {len(baselines)} baselines and {len(results)} mutations ({elapsed:.2f}s)", flush=True)
        return {
            "jobs": self.jobs, "threads": self.threads, "seconds": elapsed,
            "groups": plan.groups, "baselines": baselines,
            "mutations": [results[mutation] for mutation in plan.mutations],
        }
