# Create deterministic corrupted copies from the generated fixtures.
#
# Every corruption is byte-for-byte reproducible. The manifest records the edits
# and what a careful importer should report. Nothing here touches the originals.
#
# Usage:
#   pwsh -File .\scripts\make-corruption-fixtures.ps1
#   pwsh -File .\scripts\make-corruption-fixtures.ps1 -OutputDirectory D:\corrupt

[CmdletBinding()]
param(
    [string]$InputDirectory = '',
    [string]$OutputDirectory = ''
)

$ErrorActionPreference = 'Stop'
if (-not $InputDirectory) {
    $InputDirectory = Join-Path $PSScriptRoot '..\target\fixtures'
}
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $PSScriptRoot '..\target\corruptions'
}
$input = [IO.Path]::GetFullPath($InputDirectory)
$out = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $out | Out-Null

function Read-All([string]$Path) {
    [IO.File]::ReadAllBytes($Path)
}

function Save-All([string]$Path, [byte[]]$Bytes) {
    [IO.File]::WriteAllBytes($Path, $Bytes)
}

function Remove-Id3v2([byte[]]$Bytes) {
    $start = 0
    while (
        $Bytes.Length -ge $start + 10 -and
        $Bytes[$start] -eq 0x49 -and
        $Bytes[$start + 1] -eq 0x44 -and
        $Bytes[$start + 2] -eq 0x33
    ) {
        $size = ($Bytes[$start + 6] -shl 21) -bor
                ($Bytes[$start + 7] -shl 14) -bor
                ($Bytes[$start + 8] -shl 7) -bor
                $Bytes[$start + 9]
        $footer = if ($Bytes[$start + 3] -eq 4 -and ($Bytes[$start + 5] -band 0x10)) { 10 } else { 0 }
        $start += 10 + $size + $footer
    }
    if ($start -ge $Bytes.Length) { return [byte[]]@() }
    [byte[]]$Bytes[$start..($Bytes.Length - 1)]
}

function Remove-Range([byte[]]$Bytes, [int]$Position, [int]$Count) {
    if ($Count -le 0) { return $Bytes }
    $end = [Math]::Min($Position + $Count, $Bytes.Length)
    if ($Position -le 0) {
        if ($end -ge $Bytes.Length) { return [byte[]]@() }
        return [byte[]]$Bytes[$end..($Bytes.Length - 1)]
    }
    if ($end -ge $Bytes.Length) {
        return [byte[]]$Bytes[0..($Position - 1)]
    }
    [byte[]]($Bytes[0..($Position - 1)] + $Bytes[$end..($Bytes.Length - 1)])
}

function Insert-Bytes([byte[]]$Bytes, [int]$Position, [byte[]]$Insert) {
    if ($Position -le 0) { return [byte[]]($Insert + $Bytes) }
    if ($Position -ge $Bytes.Length) { return [byte[]]($Bytes + $Insert) }
    [byte[]]($Bytes[0..($Position - 1)] + $Insert + $Bytes[$Position..($Bytes.Length - 1)])
}

function Get-FrameLength([byte[]]$Bytes, [int]$Position) {
    if ($Position + 4 -gt $Bytes.Length) { return 0 }
    if ($Bytes[$Position] -ne 0xFF -or ($Bytes[$Position + 1] -band 0xE0) -ne 0xE0) { return 0 }
    $version = ($Bytes[$Position + 1] -shr 3) -band 0x03
    $layer = ($Bytes[$Position + 1] -shr 1) -band 0x03
    $bitrateIndex = ($Bytes[$Position + 2] -shr 4) -band 0x0F
    $rateIndex = ($Bytes[$Position + 2] -shr 2) -band 0x03
    if ($version -eq 1 -or $layer -ne 1 -or $bitrateIndex -lt 1 -or $bitrateIndex -gt 14 -or $rateIndex -eq 3) {
        return 0
    }
    $table = @(0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320)
    $shift = if ($version -eq 3) { 0 } elseif ($version -eq 2) { 1 } else { 2 }
    $rate = @(44100, 48000, 32000)[$rateIndex] -shr $shift
    $length = if ($version -eq 3) {
        [int][Math]::Floor(144000 * $table[$bitrateIndex] / $rate)
    } else {
        [int][Math]::Floor(72000 * $table[$bitrateIndex] / $rate)
    }
    if ($Bytes[$Position + 2] -band 2) { $length++ }
    $length
}

function Get-FrameOffsets([byte[]]$Bytes, [int]$Count) {
    $offsets = [System.Collections.Generic.List[int]]::new()
    $pos = 0
    while ($offsets.Count -lt $Count) {
        $length = Get-FrameLength $Bytes $pos
        $next = Get-FrameLength $Bytes ($pos + $length)
        if ($length -le 0 -or $next -le 0) {
            throw "No verified CBR frame sequence at offset $pos."
        }
        $offsets.Add($pos)
        $pos += $length
    }
    $offsets
}

$cbr = Remove-Id3v2 (Read-All (Join-Path $input 'cbr.mp3'))
$lsf = Remove-Id3v2 (Read-All (Join-Path $input 'lsf.mp3'))
if ($cbr.Length -lt 1000 -or $lsf.Length -lt 1000) {
    throw "Run scripts\generate-fixtures.ps1 first."
}
$frames = Get-FrameOffsets $cbr 60
if ($frames.Count -lt 45) {
    throw "Could not locate enough CBR frames in cbr.mp3."
}

$manifest = [System.Collections.Generic.List[string]]::new()
$manifest.Add('DirectCut RS deterministic corruption fixtures')
$manifest.Add("Source: cbr.mp3 and lsf.mp3 from $input")
$manifest.Add('Frame offsets are relative to the MPEG stream after leading tags.')
$manifest.Add('')

# A final frame cut in half.
$truncated = [byte[]]$cbr[0..($cbr.Length - 138)]
Save-All (Join-Path $out 'cbr-truncated-final.mp3') $truncated
$manifest.Add('cbr-truncated-final.mp3: last 137 bytes removed. Expect earlier frames retained and a truncated final frame reported.')

# A recording that starts partway through a frame.
$missing = Remove-Range $cbr 0 100
Save-All (Join-Path $out 'cbr-missing-start.mp3') $missing
$manifest.Add('cbr-missing-start.mp3: first 100 bytes removed. Expect resynchronization at the next verified frame sequence.')

# Junk inserted between frames.
$junk = [byte[]](@(0xA5) * 100)
$inserted = Insert-Bytes $cbr $frames[10] $junk
Save-All (Join-Path $out 'cbr-garbage-insert.mp3') $inserted
$manifest.Add("cbr-garbage-insert.mp3: 100 bytes of 0xA5 inserted at frame 11 offset $($frames[10]). Expect a stray-byte event and both sides retained.")

# One frame header damaged in its MPEG version bits.
$badHeader = [byte[]]$cbr.Clone()
$badHeader[$frames[20] + 1] = $badHeader[$frames[20] + 1] -bxor 0x08
Save-All (Join-Path $out 'cbr-bad-header.mp3') $badHeader
$manifest.Add("cbr-bad-header.mp3: MPEG version bits of frame 21 (offset $($frames[20])) flipped. Expect one invalid header, then resynchronization.")

# Bytes removed from the middle of a frame, shifting later frames earlier.
$short = Remove-Range $cbr ($frames[25] + 100) 50
Save-All (Join-Path $out 'cbr-short-frame.mp3') $short
$manifest.Add("cbr-short-frame.mp3: 50 bytes removed inside frame 26 (offset $($frames[25])). Expect a short/truncated frame, then resynchronization.")

# Whole frames removed. Without external reference this is not detectable.
$whole = Remove-Range $cbr $frames[20] ($frames[40] - $frames[20])
Save-All (Join-Path $out 'cbr-whole-frames-removed.mp3') $whole
$manifest.Add('cbr-whole-frames-removed.mp3: 20 whole frames removed at a clean boundary. The remaining stream still looks intact and is simply shorter; this loss is NOT detectable from the file alone.')

# A tag header that claims far more data than the file contains.
$bogus = [byte[]](0x49, 0x44, 0x33, 0x03, 0x00, 0x00, 0x7F, 0x7F, 0x7F, 0x7F)
$bogus += [byte[]](@(0) * 40)
$bogus += $cbr
Save-All (Join-Path $out 'cbr-id3-bogus.mp3') $bogus
$manifest.Add('cbr-id3-bogus.mp3: 10-byte ID3v2 header claiming 268435455 tag bytes, 40 zero bytes, then audio. Expect metadata diagnosis and recovered audio.')

# ID3v1 trailer.
$id3v1 = [byte[]]$cbr.Clone()
$trailer = [byte[]](@(0) * 128)
$trailer[0] = 0x54; $trailer[1] = 0x41; $trailer[2] = 0x47
$id3v1 = [byte[]]($id3v1 + $trailer)
Save-All (Join-Path $out 'cbr-id3v1.mp3') $id3v1
$manifest.Add('cbr-id3v1.mp3: 128-byte ID3v1 trailer appended. Expect metadata reported and audio unchanged.')

# Incompatible format change.
$changed = [byte[]]($cbr + $lsf)
Save-All (Join-Path $out 'cbr-format-change.mp3') $changed
$manifest.Add('cbr-format-change.mp3: MPEG-1 stereo stream followed by MPEG-2 mono stream. Expect a format-change event; streams must not be joined.')

# Leading and trailing junk.
$leading = Insert-Bytes $cbr 0 ([byte[]](@(0x11, 0x22, 0x33, 0x44, 0x55) * 7))
Save-All (Join-Path $out 'cbr-leading-junk.mp3') $leading
$manifest.Add('cbr-leading-junk.mp3: 35 junk bytes before the audio. Expect leading-byte event and resynchronization.')

$trailing = [byte[]]($cbr + [byte[]](@(0x66) * 23))
Save-All (Join-Path $out 'cbr-trailing-junk.mp3') $trailing
$manifest.Add('cbr-trailing-junk.mp3: 23 junk bytes appended. Expect trailing-byte event; audio unchanged.')

# Entirely invalid inputs.
Save-All (Join-Path $out 'empty.mp3') ([byte[]]@())
$manifest.Add('empty.mp3: zero bytes. Expect a clear error, not a panic.')
$state = 0x9E3779B9
$random = [byte[]](0..4095 | ForEach-Object {
    $state = $state -bxor (($state -shl 13) -band 0xFFFFFFFF)
    $state = $state -bxor ($state -shr 7)
    $state = $state -bxor (($state -shl 17) -band 0xFFFFFFFF)
    $state -band 0xFF
})
Save-All (Join-Path $out 'garbage.mp3') $random
$manifest.Add('garbage.mp3: 4096 deterministic pseudo-random bytes. Expect a clear error, not a panic.')

$manifestText = ($manifest -join [Environment]::NewLine) + [Environment]::NewLine
[IO.File]::WriteAllText(
    (Join-Path $out 'manifest.txt'),
    $manifestText,
    (New-Object System.Text.UTF8Encoding($false))
)
Get-ChildItem -LiteralPath $out -Filter '*.mp3' | Sort-Object Name |
    Select-Object Name, Length
