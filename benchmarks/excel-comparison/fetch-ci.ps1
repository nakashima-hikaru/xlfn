param(
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9]+$')][string]$RunId,
    [string]$Repository,
    [string]$Destination
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if ([string]::IsNullOrWhiteSpace($Destination)) {
    $Destination = Join-Path $here 'artifacts'
}
$repoRoot = (Resolve-Path (Join-Path $here '..\..')).Path
# PowerShell's current location can differ from the process working directory
# used by System.IO.Path.GetFullPath. Resolve against the caller's PS location,
# before Push-Location, and allow the destination not to exist yet.
$destinationPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination)
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
    if ($Repository -notmatch '^[A-Za-z0-9][A-Za-z0-9_.-]*/[A-Za-z0-9][A-Za-z0-9_.-]*$') {
        throw 'Repository must be OWNER/REPO.'
    }
    $runJson = gh api "repos/$Repository/actions/runs/$RunId"
    if ($LASTEXITCODE -ne 0) { throw 'Could not read CI run provenance.' }
    $run = $runJson | ConvertFrom-Json
    if ($run.event -notin @('push', 'workflow_dispatch') -or
        $run.head_repository.full_name -ne $Repository -or
        $run.head_branch -ne 'main' -or
        $run.path -ne '.github/workflows/excel-comparison-xll.yml' -or
        $run.status -ne 'completed' -or $run.conclusion -ne 'success' -or
        $run.head_sha -notmatch '^[0-9a-fA-F]{40}$') {
        throw 'Only successful main-branch XLL builds from this repository are trusted.'
    }
    gh run download $RunId --repo $Repository --name excel-comparison-x86_64 --dir $staging
    if ($LASTEXITCODE -ne 0) { throw "Could not download CI run $RunId" }
    uv run --project $here --locked excel-comparison manifest-verify --root $staging --run-id $RunId --commit $run.head_sha
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
