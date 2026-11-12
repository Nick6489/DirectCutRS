# Generate deterministic, freely creatable audio fixtures for DirectCut RS tests.
#
# Requires ffmpeg and lame on PATH. The output contains synthesized tones only;
# no copyrighted recordings and no personal archives are used.
#
# Usage:
#   pwsh -File .\scripts\generate-fixtures.ps1
#   pwsh -File .\scripts\generate-fixtures.ps1 -OutputDirectory D:\fixtures -Seconds 10

[CmdletBinding()]
param(
    [string]$OutputDirectory = '',
    [int]$Seconds = 6
)

$ErrorActionPreference = 'Stop'
if (-not $OutputDirectory) {
    # The project's one build tree, so fixtures never create another target
    # directory beside the source.
    $target = (& cargo metadata --format-version 1 --no-deps --locked |
        ConvertFrom-Json).target_directory
    $OutputDirectory = Join-Path $target 'fixtures'
}
$out = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $out | Out-Null

foreach ($tool in 'ffmpeg', 'lame') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "$tool is required on PATH. Install it, then run this script again."
    }
}

function Invoke-Tool {
    param([string]$Tool, [string[]]$Arguments)
    $native = Get-Command "$Tool.exe" -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($native) { $Tool = $native.Source }
    & $Tool @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Tool failed with exit code $LASTEXITCODE"
    }
}

function New-StereoTone {
    param([string]$Path, [string[]]$Encoder)
    # Two deterministic partials, one per channel. aevalsrc avoids depending on
    # any external sample file and keeps the fixture reproducible.
    $source = "aevalsrc=0.15*sin(2*PI*440*t)|0.12*sin(2*PI*554.37*t):s=44100:d=${Seconds}"
    $arguments = @(
        '-hide_banner', '-loglevel', 'error', '-y',
        '-f', 'lavfi', '-i', $source,
        '-map_metadata', '-1'
    ) + $Encoder + @($Path)
    Invoke-Tool ffmpeg $arguments
}

function New-MonoTone {
    param([string]$Path, [int]$Rate, [string]$Frequency, [string[]]$Encoder)
    $arguments = @(
        '-hide_banner', '-loglevel', 'error', '-y',
        '-f', 'lavfi', '-i', "sine=frequency=${Frequency}:sample_rate=${Rate}:duration=${Seconds}",
        '-ac', '1', '-map_metadata', '-1'
    ) + $Encoder + @($Path)
    Invoke-Tool ffmpeg $arguments
}

# MPEG-1, 44.1 kHz, stereo, constant bitrate.
New-StereoTone (Join-Path $out 'cbr.mp3') @('-c:a', 'libmp3lame', '-b:a', '128k')

# MPEG-1, 44.1 kHz, stereo, variable bitrate.
New-StereoTone (Join-Path $out 'vbr.mp3') @('-c:a', 'libmp3lame', '-q:a', '2')

# MPEG-2, 22.05 kHz, mono.
New-MonoTone (Join-Path $out 'lsf.mp3') 22050 '300' @('-c:a', 'libmp3lame', '-b:a', '64k')

# MPEG-2.5, 8 kHz, mono.
New-MonoTone (Join-Path $out 'mpeg25.mp3') 8000 '200' @('-c:a', 'libmp3lame', '-b:a', '32k')

# MPEG-1 with per-frame CRCs (LAME -p). LAME needs WAV input.
$wav = Join-Path $out 'crc-source.wav'
Invoke-Tool ffmpeg @(
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', "sine=frequency=500:sample_rate=44100:duration=${Seconds}",
    '-ac', '2', '-filter:a', 'volume=0.4', $wav
)
Invoke-Tool lame @('--quiet', '-b', '128', '-p', $wav, (Join-Path $out 'crc.mp3'))
Remove-Item -LiteralPath $wav -Force

# MPEG-1 Layer II, 44.1 kHz, stereo, for the self-contained frame walker.
New-StereoTone (Join-Path $out 'cbr.mp2') @('-c:a', 'mp2', '-b:a', '192k')

# ADTS AAC-LC, 44.1 kHz, stereo, for the self-contained frame walker.
New-StereoTone (Join-Path $out 'cbr.aac') @('-c:a', 'aac', '-b:a', '128k', '-f', 'adts')

# ID3v2.3 tag with embedded cover art, for metadata inventory tests.
$cover = Join-Path $out 'cover.png'
Invoke-Tool ffmpeg @(
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', 'color=c=teal:s=64x64:d=1', '-frames:v', '1', $cover
)
Invoke-Tool ffmpeg @(
    '-hide_banner', '-loglevel', 'error', '-y',
    '-i', (Join-Path $out 'cbr.mp3'), '-i', $cover,
    '-map', '0:a', '-map', '1:v', '-c:a', 'copy', '-c:v', 'png',
    '-id3v2_version', '3',
    '-metadata', 'title=Synthetic archive fixture',
    '-metadata', 'artist=DirectCut RS tests',
    '-metadata:s:v', 'title=Album cover',
    '-metadata:s:v', 'comment=Cover (front)',
    (Join-Path $out 'cbr-art.mp3')
)
Remove-Item -LiteralPath $cover -Force

Get-ChildItem -LiteralPath $out | Sort-Object Name |
    Select-Object Name, Length
