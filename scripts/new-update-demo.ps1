[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $FreshenRelease
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$freshen = (Resolve-Path -LiteralPath $FreshenRelease).Path
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'target/freshen-demo-build'
$id = [guid]::NewGuid().ToString('N').Substring(0, 12)
$demo = Join-Path $root "target/freshen-demo/$id"
$app = Join-Path $demo 'app'
$feed = Join-Path $app 'demo-feed'
$payload = Join-Path $demo 'payload'
$privateKey = Join-Path $demo 'disposable-signing.key'
$publicKey = Join-Path $demo 'disposable-signing.pub'
$previousKey = $env:DIRECTCUT_DEMO_PUBLIC_KEY
$previousVersion = $env:DIRECTCUT_DEMO_VERSION
$target = 'x86_64-pc-windows-msvc'

Push-Location -LiteralPath $root
try {
    New-Item -ItemType Directory -Path $feed, $payload | Out-Null
    & $freshen keygen $privateKey $publicKey
    $env:DIRECTCUT_DEMO_PUBLIC_KEY = (Get-Content -Raw -LiteralPath $publicKey).Trim()
    $exe = Join-Path $build "$target/release/directcut-rs.exe"

    $env:DIRECTCUT_DEMO_VERSION = '0.9.0'
    cargo build --release --locked --features freshen-demo --target $target --target-dir $build
    Copy-Item -LiteralPath $exe -Destination $app

    $env:DIRECTCUT_DEMO_VERSION = '1.0.0'
    cargo build --release --locked --features freshen-demo --target $target --target-dir $build
    Copy-Item -LiteralPath $exe -Destination $payload

    $zip = Join-Path $feed 'demo-update.zip'
    $manifest = Join-Path $feed 'freshen-manifest.json'
    & $freshen pack $payload $zip 'https://demo.invalid/demo-update.zip' $target files directcut-rs-demo '1.0.0' stable $manifest
    & $freshen sign $privateKey $manifest "$manifest.sig"
    Get-Item -LiteralPath (Join-Path $app 'directcut-rs.exe')
} finally {
    $env:DIRECTCUT_DEMO_PUBLIC_KEY = $previousKey
    $env:DIRECTCUT_DEMO_VERSION = $previousVersion
    if (Test-Path -LiteralPath $privateKey) { Remove-Item -LiteralPath $privateKey }
    Pop-Location
}
