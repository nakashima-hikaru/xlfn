"""Typer CLI for the xlfn vs Excel-DNA benchmark suite."""

from __future__ import annotations

import json
import sys
from enum import StrEnum
from pathlib import Path
from typing import Annotated

import typer

from xlfn_excel_comparison import artifact_manifest, generate_registration, runner, summarize

app = typer.Typer(
    name="excel-comparison",
    help="Windows Excel COM benchmark suite comparing xlfn and Excel-DNA.",
    no_args_is_help=True,
    add_completion=False,
)


class Profile(StrEnum):
    FULL = "full"
    SMOKE = "smoke"


class Implementation(StrEnum):
    XLFN = "xlfn"
    EXCEL_DNA = "excel_dna"


@app.command("run")
def run_cmd(
    profile: Annotated[Profile, typer.Option(help="Workload profile to run.")] = Profile.FULL,
    artifacts: Annotated[
        Path, typer.Option(help="Directory containing xlfn and excel_dna XLL artifacts.")
    ] = runner.ROOT / "artifacts",
    allow_local_artifacts: Annotated[
        bool,
        typer.Option(
            "--allow-local-artifacts",
            help="Development only: run locally built XLLs without a CI manifest.",
        ),
    ] = False,
    allow_commit_mismatch: Annotated[
        bool,
        typer.Option(
            "--allow-commit-mismatch",
            help="Run a CI XLL from a different source commit; record both revisions.",
        ),
    ] = False,
    out: Annotated[Path, typer.Option(help="Output JSONL file path.")] = runner.ROOT
    / "results.jsonl",
    append: Annotated[
        bool, typer.Option("--append", help="Append to an existing JSONL result file.")
    ] = False,
    implementation: Annotated[
        Implementation | None, typer.Option(help="Single implementation to measure.")
    ] = None,
    id: Annotated[
        list[str] | None, typer.Option("--id", help="Benchmark ID; may be repeated.")
    ] = None,
    variant: Annotated[str | None, typer.Option(help="Exact variant after ID selection.")] = None,
    repeat: Annotated[int, typer.Option(min=1, help="Measured repetitions per case.")] = 5,
    threads: Annotated[int, typer.Option(min=1, help="Excel calculation thread count.")] = 1,
    throttle_ms: Annotated[
        int, typer.Option(min=1, help="Excel RTD ThrottleInterval in ms.")
    ] = 100,
    plan: Annotated[
        bool, typer.Option("--plan", help="Print selected workload cases without launching Excel.")
    ] = False,
) -> None:
    """Run paired Excel COM benchmarks or print the execution plan."""
    selected = runner.select_cases(profile.value, id, variant)
    if not selected:
        raise typer.BadParameter("no cases matched")
    if plan:
        for case in selected:
            typer.echo(json.dumps({"key": case.key, "params": case.params}, ensure_ascii=False))
        return
    if sys.platform != "win32":
        raise typer.BadParameter("Excel execution requires Windows; --plan works on this machine")
    try:
        code = runner.run_suite(
            profile=profile.value,
            artifacts=artifacts,
            allow_local_artifacts=allow_local_artifacts,
            allow_commit_mismatch=allow_commit_mismatch,
            out=out,
            append=append,
            implementation=implementation.value if implementation else None,
            selected=selected,
            repeat=repeat,
            threads=threads,
            throttle_ms=throttle_ms,
        )
    except (FileNotFoundError, ValueError) as error:
        raise typer.BadParameter(str(error)) from error
    if code != 0:
        raise typer.Exit(code)


@app.command("worker", hidden=True)
def worker_cmd(
    profile: Annotated[Profile, typer.Option()] = Profile.FULL,
    artifacts: Annotated[Path, typer.Option()] = runner.ROOT / "artifacts",
    implementation: Annotated[Implementation, typer.Option()] = Implementation.XLFN,
    case_key: Annotated[str, typer.Option("--case-key")] = "",
    repeat: Annotated[int, typer.Option(min=1)] = 5,
    threads: Annotated[int, typer.Option(min=1)] = 1,
    throttle_ms: Annotated[int, typer.Option(min=1)] = 100,
    pid_file: Annotated[Path | None, typer.Option("--pid-file")] = None,
    progress_file: Annotated[Path | None, typer.Option("--progress-file")] = None,
) -> None:
    """Execute a single benchmark case in an isolated worker process."""
    if sys.platform != "win32":
        raise typer.BadParameter("Excel execution requires Windows")
    selected = runner.select_cases(profile.value)
    case = next((item for item in selected if item.key == case_key), None)
    if case is None or pid_file is None or progress_file is None:
        raise typer.BadParameter("invalid worker arguments")
    config = runner.WorkerConfig(
        profile=profile.value,
        artifacts=artifacts,
        implementation=implementation.value,
        repeat=repeat,
        threads=threads,
        throttle_ms=throttle_ms,
        pid_file=pid_file,
        progress_file=progress_file,
    )
    code = runner.run_worker(config, case)
    if code != 0:
        raise typer.Exit(code)


@app.command("summarize")
def summarize_cmd(
    input_path: Annotated[Path, typer.Argument(metavar="INPUT", help="Input JSONL results file.")],
    csv_out: Annotated[Path | None, typer.Option("--csv", help="Output CSV file path.")] = None,
) -> None:
    """Summarize paired xlfn vs Excel-DNA JSONL benchmark outputs into CSV."""
    output = summarize.write_summary_csv(input_path, csv_out)
    rows = summarize.summarize(summarize.load_records(input_path))
    typer.echo(f"{len(rows)} pairs -> {output}")


@app.command("generate-registration")
def generate_registration_cmd(
    count: Annotated[
        int, typer.Argument(min=0, max=5_000, help="Number of extra registration functions.")
    ] = 0,
    out: Annotated[
        Path, typer.Option("--out", help="Output C# source file path.")
    ] = generate_registration.DEFAULT_OUT,
) -> None:
    """Generate C# registration-scale functions matching xlfn/build.rs."""
    out.write_text(generate_registration.source(count), encoding="utf-8")


@app.command("manifest-create")
def manifest_create_cmd(
    root: Annotated[Path, typer.Option("--root", help="Artifact root directory.")],
    commit: Annotated[str, typer.Option("--commit", help="Git commit SHA.")],
    run_id: Annotated[str, typer.Option("--run-id", help="GitHub Actions run ID.")],
    run_attempt: Annotated[
        str, typer.Option("--run-attempt", help="GitHub Actions run attempt.")
    ] = "1",
) -> None:
    """Create manifest.json for CI-built XLL artifacts."""
    manifest = artifact_manifest.create(root, commit, run_id, run_attempt)
    typer.echo(
        f"Verified {len(manifest['files'])} {artifact_manifest.ARCHITECTURE} XLLs "
        f"from CI run {manifest['run_id']}"
    )


@app.command("manifest-verify")
def manifest_verify_cmd(
    root: Annotated[Path, typer.Option("--root", help="Artifact root directory.")],
    commit: Annotated[str | None, typer.Option("--commit", help="Expected git commit SHA.")] = None,
    run_id: Annotated[
        str | None, typer.Option("--run-id", help="Expected GitHub Actions run ID.")
    ] = None,
    require_symbols: Annotated[
        bool,
        typer.Option(
            "--require-symbols", help="Require matching baseline xlfn and Excel-DNA PDBs."
        ),
    ] = False,
) -> None:
    """Verify manifest.json and CI-built XLL artifact checksums."""
    manifest = artifact_manifest.verify(root, commit=commit, run_id=run_id)
    if require_symbols:
        artifact_manifest.verify_symbols(root, manifest, required=("xlfn", "excel_dna"))
    typer.echo(
        f"Verified {len(manifest['files'])} {artifact_manifest.ARCHITECTURE} XLLs "
        f"from CI run {manifest['run_id']}"
    )


if __name__ == "__main__":
    app()
