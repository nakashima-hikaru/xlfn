# Native x86_64 MSVC release size, build, and symbol attribution sweep.
param(
    [Parameter(Mandatory)][string] $Output,
    [string] $Repository = (Join-Path $PSScriptRoot "../.."),
    [int[]] $Counts = @(0, 128, 512, 1000)
)
$ErrorActionPreference = "Stop"
$Output = [IO.Path]::GetFullPath($Output)
$Repository = (Resolve-Path $Repository).Path
New-Item -ItemType Directory -Force $Output | Out-Null
$manifest = Join-Path $Repository "benchmarks/excel-comparison/xlfn/Cargo.toml"
$targetDir = Join-Path $Output "target"
$target = "x86_64-pc-windows-msvc"

function Read-PeSections([string] $Image) {
    $bytes = [IO.File]::ReadAllBytes($Image)
    $pe = [BitConverter]::ToInt32($bytes, 0x3c)
    if ([BitConverter]::ToUInt32($bytes, $pe) -ne 0x4550 -or [BitConverter]::ToUInt16($bytes, $pe + 4) -ne 0x8664) {
        throw "Expected an x86_64 PE image: $Image"
    }
    $count = [BitConverter]::ToUInt16($bytes, $pe + 6)
    $table = $pe + 24 + [BitConverter]::ToUInt16($bytes, $pe + 20)
    $sections = [ordered]@{}
    for ($index = 0; $index -lt $count; $index++) {
        $pos = $table + 40 * $index
        $name = [Text.Encoding]::ASCII.GetString($bytes, $pos, 8).TrimEnd([char]0)
        $sections[$name] = [ordered]@{
            virtual_bytes = [BitConverter]::ToUInt32($bytes, $pos + 8)
            file_bytes = [BitConverter]::ToUInt32($bytes, $pos + 16)
        }
    }
    return $sections
}

Push-Location $Repository
$previousCount = $env:BENCH_EXTRA_FUNCTIONS
$previousLog = $env:RUSTC_LOG
try {
    $revision = git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw "Cannot identify checkout" }
    $toolchain = rustc -Vv
    if ($LASTEXITCODE -ne 0) { throw "Cannot identify rustc" }
    $report = @()
    foreach ($count in $Counts) {
        $case = New-Item -ItemType Directory -Force (Join-Path $Output "udfs-$count")
        $map = Join-Path $case.FullName "fixture.map"
        $env:BENCH_EXTRA_FUNCTIONS = "$count"
        # rustc otherwise suppresses the successful linker's /TIME output.
        $env:RUSTC_LOG = "rustc_codegen_ssa::back::link=info"
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $cargoArgs = @("rustc", "--manifest-path", $manifest, "--release", "--locked",
            "--target", $target, "--target-dir", $targetDir, "--timings", "--",
            "-C", "link-arg=/MAP:$map", "-C", "link-arg=/TIME")
        cargo @cargoArgs 2>&1 | Set-Content (Join-Path $case.FullName "build.log")
        if ($LASTEXITCODE -ne 0) {
            Get-Content (Join-Path $case.FullName "build.log") -Tail 30
            throw "Fixture build failed ($count extras)"
        }
        $timer.Stop()
        $build = Join-Path $targetDir "$target/release"
        $image = Join-Path $build "xlfn_excel_comparison.dll"
        $pdb = Join-Path $build "xlfn_excel_comparison.pdb"
        if (!(Test-Path $map -PathType Leaf)) { throw "MSVC did not emit the requested map" }
        Copy-Item (Join-Path $targetDir "cargo-timings/cargo-timing.html") $case.FullName
        $sections = Read-PeSections $image
        $pdbBytes = (Get-Item $pdb).Length
        # Public symbol locations identify monomorphization, not exact sizes.
        $boundarySymbols = @(Select-String -Path $map -Pattern '^\s*([0-9a-f]+:[0-9a-f]+)\s+\S*sync_boundary')
        # Distinct public names can point to the same code under normal ICF.
        $boundaryAddresses = @($boundarySymbols | ForEach-Object { $_.Matches[0].Groups[1].Value } |
            Sort-Object -Unique)
        $extraExports = @(Select-String -Path $map -AllMatches -Pattern '\bxll_bench_extra_\d{4}\b' |
            ForEach-Object { $_.Matches.Value } | Sort-Object -Unique).Count
        if ($extraExports -ne $count) {
            throw "Expected $count generated UDF symbols in the map; found $extraExports"
        }
        $timingLines = @(Select-String -Path (Join-Path $case.FullName "build.log") -Pattern 'Total time|Pass [0-9]|Final:' |
            ForEach-Object { $_.Line.Trim() })
        $report += [ordered]@{
            generated_udfs = $count
            build_wall_seconds = $timer.Elapsed.TotalSeconds
            sections = $sections
            pdb_bytes = $pdbBytes
            boundary_map_entries = $boundarySymbols.Count
            boundary_map_unique_addresses = $boundaryAddresses.Count
            linker_timing = $timingLines
        }
        Write-Output "$count extras: .text=$($sections['.text'].virtual_bytes) B, PDB=$pdbBytes B, build=$($timer.Elapsed.TotalSeconds) s"
    }
    [ordered]@{ revision = $revision; rustc = $toolchain; target = $target; measurements = $report } |
        ConvertTo-Json -Depth 8 | Set-Content (Join-Path $Output "summary.json")
} finally {
    $env:BENCH_EXTRA_FUNCTIONS = $previousCount
    $env:RUSTC_LOG = $previousLog
    Pop-Location
}
