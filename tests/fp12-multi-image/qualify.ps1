# Run in a new test process; no Excel installation or workbook is used.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('i686-pc-windows-msvc', 'x86_64-pc-windows-msvc')]
    [string]$Target
)
$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '..\..')
try {
    $targetRoot = if ($env:CARGO_TARGET_DIR) {
        [System.IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
    } else {
        Join-Path (Get-Location).Path 'target'
    }
    cargo build --locked -p xlfn-host --target $Target --target-dir $targetRoot `
        --example fp12_quota_image --example fp12_quota_loader
    if ($LASTEXITCODE -ne 0) { throw "FP12 fixture compilation failed ($LASTEXITCODE)" }

    $examples = Join-Path $targetRoot "$Target\debug\examples"
    $original = Join-Path $examples 'fp12_quota_image.dll'
    $loader = Join-Path $examples 'fp12_quota_loader.exe'
    $temp = Join-Path ([System.IO.Path]::GetTempPath()) ('xlfn-fp12-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $temp | Out-Null
    try {
        $first = Join-Path $temp 'fp12_image_first.dll'
        $second = Join-Path $temp 'fp12_image_second.dll'
        Copy-Item -LiteralPath $original -Destination $first
        Copy-Item -LiteralPath $original -Destination $second
        & $loader $first $second
        if ($LASTEXITCODE -ne 0) { throw "FP12 native fixture failed ($LASTEXITCODE)" }
    } finally {
        Remove-Item -LiteralPath $temp -Recurse -Force
    }
} finally {
    Pop-Location
}
