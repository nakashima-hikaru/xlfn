param(
    [Parameter(Mandatory = $true)][string]$RunId,
    [string]$Repository,
    [string]$Destination
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if ([string]::IsNullOrWhiteSpace($Destination)) {
    $Destination = Join-Path $here 'artifacts'
}
$repoRoot = (Resolve-Path (Join-Path $here '..\..')).Path
$destinationPath = [System.IO.Path]::GetFullPath($Destination)
if (Test-Path -LiteralPath $destinationPath) {
    throw "Destination already exists: $destinationPath. Choose an unused -Destination to avoid mixing CI runs."
}
$staging = Join-Path $env:TEMP ("xlfn-excel-comparison-" + [guid]::NewGuid().ToString('N'))

Push-Location $repoRoot
try {
    if ([string]::IsNullOrWhiteSpace($Repository)) {
        $Repository = gh repo view --json nameWithOwner --jq '.nameWithOwner'
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($Repository)) {
            throw 'Could not determine the GitHub repository. Pass -Repository OWNER/REPO.'
        }
        $Repository = $Repository.Trim()
    }
    gh run download $RunId --repo $Repository --name excel-comparison-x86_64 --dir $staging
    if ($LASTEXITCODE -ne 0) { throw "Could not download CI run $RunId" }
    python (Join-Path $here 'artifact_manifest.py') verify --root $staging --run-id $RunId
    if ($LASTEXITCODE -ne 0) { throw "CI artifact verification failed for run $RunId" }
    $destinationParent = Split-Path -Parent $destinationPath
    New-Item -ItemType Directory -Force -Path $destinationParent | Out-Null
    Move-Item -LiteralPath $staging -Destination $destinationPath
    Write-Host "Verified x86_64 CI XLLs are in $destinationPath"
}
finally {
    Pop-Location
    if (Test-Path -LiteralPath $staging) {
        Remove-Item -LiteralPath $staging -Recurse -Force
    }
}
