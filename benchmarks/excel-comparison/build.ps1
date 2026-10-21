param([switch]$Smoke, [switch]$AbiSemantics)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$artifactRoot = Join-Path $here 'artifacts'
$counts = if ($Smoke) { @(10) } else { @(10, 100, 1000, 5000) }

if ($AbiSemantics) {
    # Instrumented layer-entry evidence uses a separate XLL and directory;
    # the ordinary timing fixture keeps Layers=() and has no counter overhead.
    $previousExtras = $env:BENCH_EXTRA_FUNCTIONS
    try {
        $env:BENCH_EXTRA_FUNCTIONS = '0'
        cargo build --manifest-path (Join-Path $here 'xlfn\Cargo.toml') --release --target x86_64-pc-windows-msvc --features abi-semantics --locked
        if ($LASTEXITCODE -ne 0) { throw 'ABI semantics XLL build failed' }
        $semanticsOut = Join-Path $here 'semantics-artifacts\xlfn'
        New-Item -ItemType Directory -Force -Path $semanticsOut | Out-Null
        Copy-Item (Join-Path $here 'xlfn\target\x86_64-pc-windows-msvc\release\xlfn_excel_comparison.dll') (Join-Path $semanticsOut 'abi-semantics.xll') -Force
        Write-Host "Instrumented ABI semantics XLL built in $semanticsOut"
    }
    finally { $env:BENCH_EXTRA_FUNCTIONS = $previousExtras }
    return
}

function Build-Pair([int]$extras, [string]$outputName) {
    $env:BENCH_EXTRA_FUNCTIONS = "$extras"
    try {
        cargo build --manifest-path (Join-Path $here 'xlfn\Cargo.toml') --release --target x86_64-pc-windows-msvc --locked
        if ($LASTEXITCODE -ne 0) { throw "xlfn cargo build failed ($extras)" }
        $rustDll = Join-Path $here 'xlfn\target\x86_64-pc-windows-msvc\release\xlfn_excel_comparison.dll'
        if (-not (Test-Path $rustDll)) { throw "Missing $rustDll" }
        $rustOut = Join-Path $artifactRoot 'xlfn'
        New-Item -ItemType Directory -Force -Path $rustOut | Out-Null
        Copy-Item $rustDll (Join-Path $rustOut $outputName) -Force

        python (Join-Path $here 'generate_registration.py') $extras
        if ($LASTEXITCODE -ne 0) { throw "registration source generation failed ($extras)" }
        $publishDir = Join-Path $here "excel_dna\bin\benchmark-publish\$extras"
        dotnet publish (Join-Path $here 'excel_dna\ExcelComparison.csproj') -c Release -r win-x64 -o $publishDir -p:ContinuousIntegrationBuild=true
        if ($LASTEXITCODE -ne 0) { throw "Excel-DNA NativeAOT publish failed ($extras)" }
        $packed = Join-Path $publishDir 'ExcelComparison-AddIn64.xll'
        if (-not (Test-Path -LiteralPath $packed)) { throw "64-bit NativeAOT Excel-DNA XLL not found: $packed" }
        $dnaOut = Join-Path $artifactRoot 'excel_dna'
        New-Item -ItemType Directory -Force -Path $dnaOut | Out-Null
        Copy-Item $packed (Join-Path $dnaOut $outputName) -Force
    }
    finally {
        Remove-Item Env:BENCH_EXTRA_FUNCTIONS -ErrorAction SilentlyContinue
    }
}

Build-Pair 0 'benchmark.xll'
foreach ($count in $counts) { Build-Pair $count "registration-$count.xll" }
python (Join-Path $here 'generate_registration.py') 0
if ($LASTEXITCODE -ne 0) { throw 'Could not restore baseline generated source' }
Write-Host "Paired add-ins built in $artifactRoot"
