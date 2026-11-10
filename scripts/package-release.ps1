[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $FreshenRelease,
    [Parameter(Mandatory)]
    [string] $SigningKey
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$freshen = (Resolve-Path -LiteralPath $FreshenRelease).Path
$key = (Resolve-Path -LiteralPath $SigningKey).Path
$root = Split-Path -Parent $PSScriptRoot
$target = 'x86_64-pc-windows-msvc'

Push-Location -LiteralPath $root
try {
    $metadata = cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json
    $version = ($metadata.packages | Where-Object name -eq 'directcut-rs').version
    $out = Join-Path $metadata.target_directory "dist/v$version-$Target"
    $payload = Join-Path $out 'payload'
    $zipName = "DirectCutRS-$version-$Target.zip"
    $zip = Join-Path $out $zipName
    $manifest = Join-Path $out 'freshen-manifest.json'
    $signature = "$manifest.sig"
    foreach ($path in $zip, $manifest, $signature, $payload) {
        if (Test-Path -LiteralPath $path) { throw "Release staging already exists at $path." }
    }

    cargo build --release --locked --no-default-features --target $Target
    New-Item -ItemType Directory -Path $payload | Out-Null
    $exe = Join-Path $metadata.target_directory "$Target/release/directcut-rs.exe"
    Copy-Item -LiteralPath $exe -Destination $payload
    $url = "https://github.com/Nick6489/DirectCutRS/releases/download/v$version/$zipName"
    & $freshen pack $payload $zip $url $Target files directcut-rs $version stable $manifest
    & $freshen sign $key $manifest $signature
    Get-Item -LiteralPath $zip, $manifest, $signature
} finally {
    Pop-Location
}
