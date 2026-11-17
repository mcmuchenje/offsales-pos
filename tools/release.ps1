<#
.SYNOPSIS
  Bumps the version, commits, tags and pushes, which starts the GitHub release build.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\release.ps1            # 0.2.0 -> 0.2.1
  powershell -ExecutionPolicy Bypass -File tools\release.ps1 minor      # 0.2.1 -> 0.3.0
  powershell -ExecutionPolicy Bypass -File tools\release.ps1 major      # 0.3.0 -> 1.0.0
  powershell -ExecutionPolicy Bypass -File tools\release.ps1 1.4.2      # exact version
  ... -Yes   skips the questions (commits any uncommitted changes too)
#>
param(
    [string]$Bump = "patch",
    [switch]$Yes
)
# Not "Stop": in Windows PowerShell 5.1 that turns git's normal stderr output into errors.
# Every git call is checked by its exit code instead.
$ErrorActionPreference = "Continue"
Set-Location (Split-Path $PSScriptRoot -Parent)

function Fail($msg) { Write-Host $msg -ForegroundColor Red; exit 1 }
function Confirm-Step($question) {
    if ($Yes) { return }
    $a = Read-Host "$question (y/n)"
    if ($a -notmatch '^(y|yes)$') { Fail "Stopped. Nothing was pushed." }
}
function Invoke-Git {
    & git @args
    if ($LASTEXITCODE -ne 0) { Fail "git $($args -join ' ') failed." }
}

# --- checks -----------------------------------------------------------------
if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Fail "git isn't installed or isn't on PATH." }
git rev-parse --is-inside-work-tree *> $null
if ($LASTEXITCODE -ne 0) { Fail "This folder isn't a git repository." }
$remote = git remote get-url origin 2>$null
if (-not $remote) { Fail "No 'origin' remote. Add one with: git remote add origin <url>" }
$branch = git branch --show-current
if (-not $branch) { Fail "You're not on a branch (detached HEAD). Check out main first." }

# --- work out the new version -----------------------------------------------
$utf8 = New-Object System.Text.UTF8Encoding($false)
$tomlPath = Join-Path (Get-Location) "Cargo.toml"
$lockPath = Join-Path (Get-Location) "Cargo.lock"
$toml = [IO.File]::ReadAllText($tomlPath)
$m = [regex]::Match($toml, '(?m)^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"')
if (-not $m.Success) { Fail "Couldn't find the version in Cargo.toml." }
$current = "$($m.Groups[1]).$($m.Groups[2]).$($m.Groups[3])"
[int]$maj = $m.Groups[1].Value; [int]$min = $m.Groups[2].Value; [int]$pat = $m.Groups[3].Value

switch -Regex ($Bump) {
    '^patch$' { $new = "$maj.$min.$($pat + 1)" }
    '^minor$' { $new = "$maj.$($min + 1).0" }
    '^major$' { $new = "$($maj + 1).0.0" }
    '^\d+\.\d+\.\d+$' { $new = $Bump }
    default { Fail "Use patch, minor, major, or a version like 1.2.3 (got '$Bump')." }
}
if ([version]$new -lt [version]$current) { Fail "$new is older than the current version $current." }
$tag = "v$new"

if (git tag --list $tag) { Fail "Tag $tag already exists locally. Pick another version." }
git fetch --tags origin 2>$null
if (git ls-remote --tags origin "refs/tags/$tag") { Fail "Tag $tag already exists on GitHub. Pick another version." }

Write-Host ""
Write-Host "Release $current -> $new  (tag $tag, branch $branch, remote $remote)" -ForegroundColor Cyan

# --- uncommitted changes ------------------------------------------------------
$dirty = git status --porcelain
if ($dirty) {
    Write-Host "`nThese uncommitted changes will be included in the release:" -ForegroundColor Yellow
    $dirty | ForEach-Object { Write-Host "  $_" }
    Confirm-Step "Include them?"
}

# --- bump --------------------------------------------------------------------
if ($new -ne $current) {
    $toml = [regex]::Replace($toml, '(?m)^(version\s*=\s*")[^"]+(")', "`${1}$new`${2}", 1)
    [IO.File]::WriteAllText($tomlPath, $toml, $utf8)
    if (Test-Path $lockPath) {
        $lock = [IO.File]::ReadAllText($lockPath)
        $lock = [regex]::Replace($lock, '(name = "offsales-pos"\r?\nversion = ")[^"]+(")', "`${1}$new`${2}", 1)
        [IO.File]::WriteAllText($lockPath, $lock, $utf8)
    }
}

Confirm-Step "Commit, tag $tag and push to GitHub?"

Invoke-Git add -A
git diff --cached --quiet
if ($LASTEXITCODE -ne 0) { Invoke-Git commit -m "$tag" }
Invoke-Git tag -a $tag -m "$tag"
Invoke-Git push origin $branch
Invoke-Git push origin $tag

# Link to the Actions page if origin is a GitHub URL.
$repo = $remote -replace '^(https://github\.com/|git@github\.com:)', '' -replace '\.git$', ''
Write-Host ""
Write-Host "Pushed $tag. GitHub is now building the release (about 10-15 minutes)." -ForegroundColor Green
if ($repo -ne $remote) {
    Write-Host "  Progress: https://github.com/$repo/actions"
    Write-Host "  Release:  https://github.com/$repo/releases/tag/$tag"
}
Write-Host "Then update the server first, then the tills (Settings -> Updates, or the 'Update ready' button)."
