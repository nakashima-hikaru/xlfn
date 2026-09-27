param([switch]$Smoke)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$artifactRoot = Join-Path $here 'artifacts'
$counts = if ($Smoke) { @(10) } else { @(10, 100, 1000, 5000) }

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
        $buildStart = Get-Date
        dotnet build (Join-Path $here 'excel_dna\ExcelComparison.csproj') -c Release --no-incremental -p:ContinuousIntegrationBuild=true
        if ($LASTEXITCODE -ne 0) { throw "Excel-DNA build failed ($extras)" }
        $packed = Get-ChildItem (Join-Path $here 'excel_dna\bin\Release') -Recurse -File -Filter '*64-packed.xll' |
            Where-Object { $_.LastWriteTime -ge $buildStart.AddSeconds(-2) } |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if ($null -eq $packed) { throw '64-bit packed Excel-DNA XLL not found' }
        $dnaOut = Join-Path $artifactRoot 'excel_dna'
        New-Item -ItemType Directory -Force -Path $dnaOut | Out-Null
        Copy-Item $packed.FullName (Join-Path $dnaOut $outputName) -Force
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
