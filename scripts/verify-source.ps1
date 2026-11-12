# Non-destructive validation of the DirectCut RS source and existing fixtures.
# Run on Windows from any directory. Does not modify source recordings.
[CmdletBinding()]
param(
    [switch]$WithFixtures,
    [switch]$ReleaseBuild
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $root
try {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'Rust cargo is required on PATH (Windows MSVC toolchain).'
    }
    function Invoke-Cargo {
        param([string[]]$Arguments)
        & cargo @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "cargo $($Arguments -join ' ') failed with code $LASTEXITCODE"
        }
    }
    Write-Host 'Checking the locked dependency graph...'
    Invoke-Cargo -Arguments @('metadata', '--format-version', '1', '--no-deps', '--locked') | Out-Null
    Write-Host 'Running regression tests...'
    Invoke-Cargo -Arguments @('test', '--locked')
    if ($ReleaseBuild) {
        Write-Host 'Building the portable release executable...'
        Invoke-Cargo -Arguments @('build', '--release', '--locked')
    }
    if ($WithFixtures) {
        foreach ($tool in 'ffmpeg', 'lame') {
            if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
                throw "$tool is required for generated fixtures."
            }
        }
        & (Join-Path $PSScriptRoot 'generate-fixtures.ps1')
        if (-not $?) { throw 'Fixture generation failed.' }
        & (Join-Path $PSScriptRoot 'make-corruption-fixtures.ps1')
        if (-not $?) { throw 'Corruption-fixture generation failed.' }
        # The fixture scripts resolve the build tree from cargo metadata, so
        # the fixture paths follow the same single target directory.
        $metadata = cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json
        $target = $metadata.target_directory
        $env:DIRECTCUT_TEST_FIXTURES = Join-Path $target 'fixtures'
        $env:DIRECTCUT_TEST_CORRUPTIONS = Join-Path $target 'corruptions'
        try {
            Write-Host 'Running optional recovery and trim fixtures...'
            Invoke-Cargo -Arguments @('test', '--locked', '--', '--ignored', '--skip', 'export_benchmark')
        } finally {
            Remove-Item Env:DIRECTCUT_TEST_FIXTURES -ErrorAction SilentlyContinue
            Remove-Item Env:DIRECTCUT_TEST_CORRUPTIONS -ErrorAction SilentlyContinue
        }
    }
    Write-Host 'Source validation commands completed successfully.'
} finally {
    Pop-Location
}
