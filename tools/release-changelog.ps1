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

    It only rewrites the changelog. Committing and tagging are left to you.
    Run this before you commit + tag a release.

.PARAMETER Version
    Override the version instead of reading it from Cargo.toml.

.PARAMETER DryRun
    Print what the new changelog would look like without writing it.

.EXAMPLE
    ./tools/release-changelog.ps1
    Rolls Unreleased into a section named after the Cargo.toml version.

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
if (-not $Version) {
    if (-not (Test-Path $cargoPath)) {
        throw "Cargo.toml not found at $cargoPath (pass -Version to override)"
    }
    $cargoLines = Get-Content $cargoPath
    $inPackage = $false
    foreach ($line in $cargoLines) {
        if ($line -match '^\s*\[package\]\s*$') { $inPackage = $true; continue }
        # A new table header ends the [package] section.
        if ($inPackage -and $line -match '^\s*\[') { break }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            $Version = $Matches[1]
            break
        }
    }
    if (-not $Version) {
        throw "Could not find version in [package] section of Cargo.toml (pass -Version to override)"
    }
}

Write-Host "Releasing changelog for version $Version" -ForegroundColor Cyan

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
    return
}

# Write UTF-8 without BOM.
[System.IO.File]::WriteAllText($changelogPath, $output, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "changelog.md updated: moved Unreleased entries into ## $Version." -ForegroundColor Green
Write-Host "Review the diff, then commit and tag when you're ready." -ForegroundColor Green
