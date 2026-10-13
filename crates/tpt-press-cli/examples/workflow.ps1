# A walkthrough of the tpt-press command line, for PowerShell.
#
# It builds a small set of sample inputs in a temporary folder, packs and
# unpacks each kind of archive, and checks that the round trips are exact.
#
#   cargo build --release -p tpt-press-cli
#   .\crates\tpt-press-cli\examples\workflow.ps1
#
# Set $env:TPT_PRESS to use a different binary.

$ErrorActionPreference = 'Stop'
$tpt = if ($env:TPT_PRESS) { $env:TPT_PRESS } else { Join-Path $PSScriptRoot '..\..\..\target\release\tpt-press.exe' }
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("tpt-press-workflow-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null

# Writes lines with LF endings. Set-Content uses CRLF on Windows, and the
# genomics parser refuses CRLF input.
function Write-Lf($path, $lines) {
    [System.IO.File]::WriteAllText($path, (($lines -join "`n") + "`n"))
}

function Assert-Same($a, $b, $what) {
    if ((Get-FileHash $a).Hash -ne (Get-FileHash $b).Hash) { throw "$what differs after a round trip" }
    Write-Host "  ok: $what matches the original"
}

try {
    # 1. Generic archive: any file, zstd compressed.
    Write-Host "== generic archive"
    $plain = Join-Path $work 'notes.txt'
    Write-Lf $plain (1..20000 | ForEach-Object { "line $_ the quick brown fox" })
    & $tpt pack $plain (Join-Path $work 'notes.tppr') --level 9
    & $tpt inspect (Join-Path $work 'notes.tppr')
    & $tpt unpack (Join-Path $work 'notes.tppr') (Join-Path $work 'notes.out.txt')
    Assert-Same $plain (Join-Path $work 'notes.out.txt') 'notes.txt'

    # 2. Logs: JSON Lines, time indexed. Query a window and two columns.
    Write-Host "== logs"
    $jsonl = Join-Path $work 'app.jsonl'
    $start = [DateTime]::new(2026, 3, 1, 0, 0, 0, [DateTimeKind]::Utc)
    $records = 0..2879 | ForEach-Object {
        $ts = $start.AddSeconds($_ * 30).ToString("yyyy-MM-dd'T'HH:mm:ss'Z'")
        $level = if ($_ % 10 -eq 0) { 'warn' } else { 'info' }
        "{`"timestamp`":`"$ts`",`"level`":`"$level`",`"ms`":$($_ % 500)}"
    }
    Write-Lf $jsonl $records
    & $tpt pack-logs $jsonl (Join-Path $work 'app.tplg') --format jsonl --timestamp-field timestamp
    & $tpt query (Join-Path $work 'app.tplg') --from 2026-03-01T01:00:00Z --to 2026-03-01T01:02:00Z --columns timestamp,level

    # 3. Genomics: FASTA with random access by record name.
    Write-Host "== genomics"
    $fasta = Join-Path $work 'reads.fna'
    Write-Lf $fasta @('>read1 sample', 'ACGTACGTTAGC', 'GGATCCAT', '>read2 sample', 'TTGACCAGTAAC')
    & $tpt pack-genomics $fasta (Join-Path $work 'reads.tpgn')
    & $tpt fetch-genomics (Join-Path $work 'reads.tpgn') read2 --start 3 --end 8
    & $tpt unpack-genomics (Join-Path $work 'reads.tpgn') (Join-Path $work 'reads.out.fna')
    Assert-Same $fasta (Join-Path $work 'reads.out.fna') 'reads.fna'

    # 4. Assets: pack a folder into one archive, list it, and unpack it.
    Write-Host "== assets"
    $assets = Join-Path $work 'assets'
    New-Item -ItemType Directory -Path (Join-Path $assets 'ui') | Out-Null
    Write-Lf (Join-Path $assets 'ui\button.txt') @('button' * 400)
    Write-Lf (Join-Path $assets 'level.dat') @('level data')
    & $tpt pack-assets $assets (Join-Path $work 'game.tpa') --store
    & $tpt list-assets (Join-Path $work 'game.tpa') --verify
    & $tpt unpack-assets (Join-Path $work 'game.tpa') (Join-Path $work 'assets-out')
    Assert-Same (Join-Path $assets 'level.dat') (Join-Path $work 'assets-out\level.dat') 'level.dat'

    # 5. Encryption to a key pair. Only the holder of the identity can open it.
    Write-Host "== encryption"
    # keygen writes the secret identity to a file, and prints the public key,
    # which is what senders need.
    $public = (& $tpt keygen (Join-Path $work 'me.key') | Where-Object { $_ -like 'public key: *' }) -replace '^public key: ', ''
    & $tpt pack $plain (Join-Path $work 'secret.tppr') --recipient $public
    & $tpt unpack (Join-Path $work 'secret.tppr') (Join-Path $work 'secret.out.txt') --identity (Join-Path $work 'me.key')
    Assert-Same $plain (Join-Path $work 'secret.out.txt') 'encrypted notes.txt'

    Write-Host "`nall round trips passed"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
