<#
.SYNOPSIS
    Rolls the "Unreleased" changelog entries into a versioned section.

.DESCRIPTION
    Reads the package version from Cargo.toml, then rewrites changelog.md:
      - Everything currently under "## Unreleased" is moved into a new
        "## <version>" section inserted directly below Unreleased.
      - The Unreleased section keeps its sub-headings (### Additions,
        ### Fixes, ### Changes, in their existing order) but is left empty,
        ready for the next round of entries.

    When -Version names a version other than the one in Cargo.toml, the
    [package] version there is bumped to match, so the changelog heading and
    the crate version can't drift apart. Cargo.lock is refreshed too.

    It only rewrites those files. Committing and tagging are left to you.
    Run this before you commit + tag a release.

.PARAMETER Version
    Release under this version instead of the one in Cargo.toml, and bump
    Cargo.toml to match. Instead of a literal version you can pass:
      +    bump the patch number   (0.1.4 -> 0.1.5)
      ++   bump the minor number   (0.1.4 -> 0.2.0)
      +++  bump the major number   (0.1.4 -> 1.0.0)
    Each of these starts from the Cargo.toml version and zeroes everything to
    the right of what it raised.

.PARAMETER DryRun
    Print what the new changelog would look like without writing it, and
    report the Cargo.toml bump without making it.

.EXAMPLE
    ./tools/release-changelog.ps1
    Rolls Unreleased into a section named after the Cargo.toml version.

.EXAMPLE
    ./tools/release-changelog.ps1 -Version 0.2.0
    Bumps Cargo.toml to 0.2.0 and rolls Unreleased into a "## 0.2.0" section.

.EXAMPLE
    ./tools/release-changelog.ps1 -Version +
    Bumps the patch number (0.1.4 -> 0.1.5) and releases under it.

.EXAMPLE
    ./tools/release-changelog.ps1 -DryRun
    Shows the result without touching the file.
#>
[CmdletBinding()]
param(
    [string]$Version,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Resolve paths relative to this script so it works from any working directory.
$repoRoot     = Split-Path -Parent $PSScriptRoot
$cargoPath    = Join-Path $repoRoot 'Cargo.toml'
$changelogPath = Join-Path $repoRoot 'changelog.md'

if (-not (Test-Path $changelogPath)) {
    throw "changelog.md not found at $changelogPath"
}

# --- Determine the version -------------------------------------------------
# Read Cargo.toml's [package] version even when -Version was passed: we need it
# to decide whether the crate has to be bumped, and its line index to do so.
$cargoVersion = $null
$cargoVersionLine = -1
$cargoLines = @()
if (Test-Path $cargoPath) {
    # Same UTF-8 care as the changelog below; we rewrite this file too.
    $cargoRaw = [System.IO.File]::ReadAllText($cargoPath, [System.Text.Encoding]::UTF8)
    $cargoNewline = if ($cargoRaw -match "`r`n") { "`r`n" } else { "`n" }
    $cargoLines = $cargoRaw -split "`r?`n"
    $inPackage = $false
    for ($i = 0; $i -lt $cargoLines.Count; $i++) {
        $line = $cargoLines[$i]
        if ($line -match '^\s*\[package\]\s*$') { $inPackage = $true; continue }
        # A new table header ends the [package] section.
        if ($inPackage -and $line -match '^\s*\[') { break }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            $cargoVersion = $Matches[1]
            $cargoVersionLine = $i
            break
        }
    }
}

if (-not $Version) {
    if (-not (Test-Path $cargoPath)) {
        throw "Cargo.toml not found at $cargoPath (pass -Version to override)"
    }
    if (-not $cargoVersion) {
        throw "Could not find version in [package] section of Cargo.toml (pass -Version to override)"
    }
    $Version = $cargoVersion
} elseif ($Version -match '^\++$') {
    # "+" / "++" / "+++": bump patch / minor / major of the Cargo.toml version.
    # More plus signs mean a bigger step, so the count reads left from the end.
    if (-not $cargoVersion) {
        throw "Cannot use '$Version' - no version found in the [package] section of Cargo.toml to bump from"
    }
    if ($Version.Length -gt 3) {
        throw "Unrecognised -Version '$Version' - use +, ++, or +++ (patch, minor, major)"
    }
    if ($cargoVersion -notmatch '^(\d+)\.(\d+)\.(\d+)$') {
        throw "Cannot bump '$cargoVersion' with '$Version' - it is not a plain major.minor.patch version"
    }
    $parts = @([int]$Matches[1], [int]$Matches[2], [int]$Matches[3])
    # Index of the component to raise; everything to its right resets to 0.
    $slot = 3 - $Version.Length
    $parts[$slot] += 1
    for ($i = $slot + 1; $i -lt 3; $i++) { $parts[$i] = 0 }
    $Version = $parts -join '.'
}

# Bump Cargo.toml only when the caller named a different version. Doing it here
# (before the changelog is parsed) would strand a bumped crate if the changelog
# turns out to have nothing to release, so the write itself happens at the end.
$bumpCargo = $cargoVersion -and ($cargoVersion -ne $Version)
if ($bumpCargo -and $cargoVersionLine -lt 0) {
    throw "Cannot bump Cargo.toml to $Version - no version line found in its [package] section"
}

Write-Host "Releasing changelog for version $Version" -ForegroundColor Cyan
if ($bumpCargo) {
    Write-Host "Cargo.toml will be bumped from $cargoVersion to $Version" -ForegroundColor Cyan
}

# --- Parse the changelog ---------------------------------------------------
# Read as UTF-8 explicitly: Windows PowerShell 5.1's Get-Content defaults to the
# ANSI codepage and would mangle em-dashes etc., which we'd then double-encode
# on write. ReadAllText handles a UTF-8 BOM if present and assumes UTF-8 otherwise.
$raw = [System.IO.File]::ReadAllText($changelogPath, [System.Text.Encoding]::UTF8)
# Preserve the file's newline style; default to CRLF on Windows.
$newline = if ($raw -match "`r`n") { "`r`n" } else { "`n" }
$lines = $raw -split "`r?`n"

# Locate the Unreleased section: from "## Unreleased" up to the next "## " (or EOF).
$startIdx = -1
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match '^##\s+Unreleased\s*$') { $startIdx = $i; break }
}
if ($startIdx -lt 0) {
    throw "No '## Unreleased' section found in changelog.md"
}

$endIdx = $lines.Count  # exclusive; default to EOF
for ($i = $startIdx + 1; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match '^##\s+(?!#)') { $endIdx = $i; break }
}

# Body = everything strictly between the "## Unreleased" line and the next "## ".
$bodyStart = $startIdx + 1
$bodyLines = @()
if ($endIdx -gt $bodyStart) {
    $bodyLines = $lines[$bodyStart..($endIdx - 1)]
}

# Refuse if a section for this version already exists.
$versionHeading = '^##\s+' + [regex]::Escape($Version) + '\s*$'
foreach ($line in $lines) {
    if ($line -match $versionHeading) {
        throw "changelog.md already has a '## $Version' section - nothing to do."
    }
}

# Collect sub-heading lines (### ...) in order, and detect whether there are
# any actual content lines (a non-blank line that isn't a sub-heading).
$subHeadings = @()
$hasContent = $false
foreach ($line in $bodyLines) {
    if ($line -match '^###\s+') {
        $subHeadings += $line
    } elseif ($line.Trim() -ne '') {
        $hasContent = $true
    }
}

if (-not $hasContent) {
    Write-Host "Unreleased section has no entries - nothing to release." -ForegroundColor Yellow
    return
}

# --- Rebuild the section ---------------------------------------------------
# Trim leading/trailing blank lines from the moved body for tidy output.
$moved = [System.Collections.Generic.List[string]]::new()
foreach ($l in $bodyLines) { $moved.Add($l) }
while ($moved.Count -gt 0 -and $moved[0].Trim() -eq '') { $moved.RemoveAt(0) }
while ($moved.Count -gt 0 -and $moved[$moved.Count - 1].Trim() -eq '') { $moved.RemoveAt($moved.Count - 1) }

# New empty Unreleased body: each sub-heading followed by a blank line.
$emptyBody = [System.Collections.Generic.List[string]]::new()
foreach ($h in $subHeadings) {
    $emptyBody.Add('')
    $emptyBody.Add($h)
}
$emptyBody.Add('')

# Assemble: [before Unreleased line] + "## Unreleased" + empty body
#           + "## <version>" + moved body + [rest of file]
$result = [System.Collections.Generic.List[string]]::new()
for ($i = 0; $i -le $startIdx; $i++) { $result.Add($lines[$i]) }   # through "## Unreleased"
foreach ($l in $emptyBody) { $result.Add($l) }
$result.Add("## $Version")
$result.Add('')
foreach ($l in $moved) { $result.Add($l) }
$result.Add('')
for ($i = $endIdx; $i -lt $lines.Count; $i++) { $result.Add($lines[$i]) }

$output = ($result -join $newline)
# Guarantee exactly one trailing newline.
$output = $output.TrimEnd("`r", "`n") + $newline

if ($DryRun) {
    Write-Host "--- DryRun: new changelog.md would be ---`n" -ForegroundColor Cyan
    Write-Output $output
    if ($bumpCargo) {
        Write-Host "`n--- DryRun: Cargo.toml line $($cargoVersionLine + 1) would become ---" -ForegroundColor Cyan
        Write-Output "version = `"$Version`""
    }
    return
}

# Write UTF-8 without BOM.
[System.IO.File]::WriteAllText($changelogPath, $output, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "changelog.md updated: moved Unreleased entries into ## $Version." -ForegroundColor Green

# --- Bump Cargo.toml -------------------------------------------------------
if ($bumpCargo) {
    # Rewrite just the one line, preserving whatever spacing it had.
    $cargoLines[$cargoVersionLine] = $cargoLines[$cargoVersionLine] -replace '"[^"]+"', "`"$Version`""
    $cargoOut = ($cargoLines -join $cargoNewline).TrimEnd("`r", "`n") + $cargoNewline
    [System.IO.File]::WriteAllText($cargoPath, $cargoOut, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Cargo.toml updated: version = `"$Version`"." -ForegroundColor Green

    # Keep Cargo.lock in step so the release build doesn't produce a stray
    # lockfile change. Offline: the only thing that moved is our own package.
    try {
        Push-Location $repoRoot
        cargo update --offline --package pubsplash --quiet
        if ($LASTEXITCODE -ne 0) { throw "cargo update exited with $LASTEXITCODE" }
        Write-Host "Cargo.lock refreshed." -ForegroundColor Green
    } catch {
        Write-Host "Could not refresh Cargo.lock ($_) - run 'cargo update -p pubsplash' yourself." -ForegroundColor Yellow
    } finally {
        Pop-Location
    }
}

Write-Host "Review the diff, then commit and tag when you're ready." -ForegroundColor Green
