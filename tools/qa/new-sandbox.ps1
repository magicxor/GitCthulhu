<#
.SYNOPSIS
  Creates a QA sandbox for the qa build of GitCthulhu: an app-data directory plus deterministic fixture
  repositories built with the git executable.

.DESCRIPTION
  The sandbox is the only place a qa build may touch (crates/gitcthulhu/src/qa/gate.rs): the app refuses to run
  unless every data location points into a sandbox that carries the .qa-sandbox marker written here.

  Layout of <Dest>:
    .qa-sandbox          marker (first line: gitcthulhu-qa-sandbox v1)
    gitconfig            the only global Git configuration the app and the fixtures see
    app\                 GITCTHULHU_HOME: settings.json, state.json, repos.json, logs, caches
    profile\             USERPROFILE / HOME / APPDATA / LOCALAPPDATA of the app process
    repos\linear         a small linear history, clean working tree
    repos\rich           branches, merge commits, tags, stashes, a local bare remote, ahead and behind commits,
                         and a merge in progress with staged, unstaged, untracked and conflicted files
    repos\rich-remote.git the bare remote of "rich"
    repos\rebase         a rebase stopped by a conflict (step 1 of 3)
    out\                 default output of run-gui.ps1

  All names, e-mail addresses and file contents are synthetic. Commit dates are fixed, so the history and the
  log graph are identical on every run (object ids differ only if the git version changes how it writes them).
  The sandbox must live below a folder named "scratch" or below the temp folder. An existing sandbox is only
  replaced with -Force (and only if it carries the marker); any other existing folder is never touched.

.PARAMETER Dest
  The sandbox directory to create.

.PARAMETER Force
  Replace an existing sandbox (recognized by its marker).

.EXAMPLE
  pwsh -NoProfile -File tools\qa\new-sandbox.ps1 -Dest C:\git\opensource\GitCthulhu.plan\scratch\qa1\sbx
#>
param(
    [Parameter(Mandatory = $true)][string]$Dest,
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'qa-common.psm1') -Force

$Dest = Assert-UnderScratch $Dest
if (Test-Path -LiteralPath $Dest) {
    if (-not (Test-QaSandbox $Dest)) {
        throw "'$Dest' exists and is not a QA sandbox (no marker); refusing to touch it."
    }
    if (-not $Force) { throw "'$Dest' is already a sandbox; use -Force to recreate it." }
    Remove-Item -LiteralPath $Dest -Recurse -Force
}

$git = (Get-Command git -ErrorAction Stop).Source
New-Item -ItemType Directory -Force -Path $Dest | Out-Null
foreach ($sub in 'app', 'repos', 'out', 'profile\AppData\Roaming', 'profile\AppData\Local', 'profile\.config', 'profile\.local\share', 'profile\.cache') {
    New-Item -ItemType Directory -Force -Path (Join-Path $Dest $sub) | Out-Null
}
$repos = Join-Path $Dest 'repos'

# The one global Git configuration of the sandbox (synthetic identity, no signing, no system configuration).
$gitConfig = @"
[user]
	name = QA Author
	email = qa.author@example.invalid
[init]
	defaultBranch = main
[core]
	autocrlf = false
	longpaths = true
[commit]
	gpgsign = false
[tag]
	gpgsign = false
[pull]
	rebase = false
[advice]
	detachedHead = false
[safe]
	directory = *
[protocol "file"]
	allow = always
"@
[IO.File]::WriteAllText((Join-Path $Dest 'gitconfig'), ($gitConfig -replace "`r`n", "`n"), [Text.UTF8Encoding]::new($false))

# ---------------------------------------------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------------------------------------------
$script:Tick = 0
$people = @(
    @('Alice Example', 'alice@example.invalid'),
    @('Bob Example', 'bob@example.invalid'),
    @('Carol Example', 'carol@example.invalid')
)

function Invoke-Git {
    param([string]$Repo, [string[]]$GitArgs, [switch]$AllowFailure)
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { $output = & $git -C $Repo @GitArgs 2>&1 | ForEach-Object { "$_" } } finally { $ErrorActionPreference = $previous }
    if ($LASTEXITCODE -ne 0 -and -not $AllowFailure) {
        throw "git $($GitArgs -join ' ') failed in $Repo (exit $LASTEXITCODE):`n$($output -join "`n")"
    }
    $output
}

function Set-Actor {
    param([int]$Index, [switch]$AdvanceClock)
    if ($AdvanceClock) { $script:Tick++ }
    $person = $people[$Index % $people.Count]
    $date = ([datetime]'2024-03-01T09:00:00Z').AddHours($script:Tick * 7).ToString('yyyy-MM-ddTHH:mm:ssZ')
    $env:GIT_AUTHOR_NAME = $person[0]; $env:GIT_AUTHOR_EMAIL = $person[1]; $env:GIT_AUTHOR_DATE = $date
    $env:GIT_COMMITTER_NAME = $person[0]; $env:GIT_COMMITTER_EMAIL = $person[1]; $env:GIT_COMMITTER_DATE = $date
}

function New-Commit {
    # Stages everything and commits with the next fixed date and the next author.
    param([string]$Repo, [string]$Message, [int]$Actor = 0)
    Set-Actor -Index $Actor -AdvanceClock
    Invoke-Git $Repo @('add', '-A') | Out-Null
    Invoke-Git $Repo @('commit', '-q', '-m', $Message) | Out-Null
}

function Write-Text {
    param([string]$Repo, [string]$Path, [string[]]$Lines)
    $full = Join-Path $Repo $Path
    New-Item -ItemType Directory -Force -Path (Split-Path $full) | Out-Null
    [IO.File]::WriteAllText($full, (($Lines -join "`n") + "`n"), [Text.UTF8Encoding]::new($false))
}

function Get-Source {
    # A synthetic source-like file of $Count lines; $Edits maps a 1-based line number to replacement text.
    param([string]$Module, [int]$Count = 40, [hashtable]$Edits = @{})
    1..$Count | ForEach-Object {
        if ($Edits.ContainsKey($_)) { $Edits[$_] }
        elseif ($_ % 10 -eq 1) { "fn $($Module)_step_$($_)(input: &Input) -> Output {" }
        elseif ($_ % 10 -eq 0) { '}' }
        elseif ($_ % 10 -eq 2) { "    // $Module step $_ prepares the state" }
        else { "    let value_$_ = compute($_, input.seed);" }
    }
}

function New-Repo {
    param([string]$Name)
    $path = Join-Path $repos $Name
    New-Item -ItemType Directory -Force -Path $path | Out-Null
    Invoke-Git $path @('init', '-q', '-b', 'main') | Out-Null
    $path
}

# ---------------------------------------------------------------------------------------------------------------
# Fixtures (run with the sandbox's Git environment and fixed identities)
# ---------------------------------------------------------------------------------------------------------------
$environment = Get-QaEnvironment -Sandbox $Dest
Invoke-WithEnvironment -Variables $environment -Script {

    # --- linear: six commits on main, clean working tree ---
    $linear = New-Repo 'linear'
    Write-Text $linear 'README.md' @('# Linear', '', 'A small linear repository for QA screenshots.')
    Write-Text $linear 'src/main.txt' (Get-Source 'main' 30)
    New-Commit $linear 'Initial commit' 0
    Write-Text $linear 'docs/notes.md' @('# Notes', '', '- first note', '- second note')
    New-Commit $linear 'Add project notes' 1
    Write-Text $linear 'src/main.txt' (Get-Source 'main' 30 @{ 3 = '    let value_3 = compute(3, input.seed) + 1;'; 14 = '    let value_14 = refine(14);'; 25 = '    let value_25 = refine(25);' })
    New-Commit $linear 'Tune the main loop' 2
    Write-Text $linear 'src/util.txt' (Get-Source 'util' 24)
    New-Commit $linear 'Add the util module' 0
    Write-Text $linear 'docs/notes.md' @('# Notes', '', '- first note', '- second note', '- third note, with a longer line that explains what the note is about in some detail')
    New-Commit $linear 'Extend the notes' 1
    Write-Text $linear 'README.md' @('# Linear', '', 'A small linear repository for QA screenshots.', '', 'See docs/notes.md.')
    New-Commit $linear 'Link the notes from the README' 2

    # --- rich: branches, merges, tags, stashes, a remote, ahead/behind, a merge in progress ---
    $remote = Join-Path $repos 'rich-remote.git'
    Invoke-Git $repos @('init', '-q', '--bare', '-b', 'main', $remote) | Out-Null
    $rich = New-Repo 'rich'
    Write-Text $rich 'README.md' @('# Rich', '', 'Branches, tags, stashes, a remote and a merge in progress.')
    Write-Text $rich 'src/app.txt' (Get-Source 'app' 40)
    Write-Text $rich 'docs/guide.md' @('# Guide', '', 'Install the tool.', 'Configure the tool.', 'Run the tool.', 'Report problems.')
    Write-Text $rich 'docs/legacy.md' @('# Legacy', '', 'This page is obsolete.')
    [IO.File]::WriteAllBytes((Join-Path $rich 'assets-logo.bin'), [byte[]](0..255))
    New-Commit $rich 'Initial commit' 0
    Write-Text $rich 'src/settings.txt' (Get-Source 'settings' 28)
    New-Commit $rich 'Add the settings module' 1
    Write-Text $rich 'docs/guide.md' @('# Guide', '', 'Install the tool.', 'Configure the tool.', 'Run the tool.', 'Report problems here.')
    New-Commit $rich 'Fix wording in the guide' 2
    Set-Actor 0 -AdvanceClock
    Invoke-Git $rich @('tag', '-a', 'v0.1.0', '-m', 'First release') | Out-Null

    Invoke-Git $rich @('checkout', '-q', '-b', 'fix/typo') | Out-Null
    Write-Text $rich 'README.md' @('# Rich', '', 'Branches, tags, stashes, a remote and a merge in progress!')
    New-Commit $rich 'Fix a typo in the README' 1
    Write-Text $rich 'docs/legacy.md' @('# Legacy', '', 'This page is obsolete and will be removed.')
    New-Commit $rich 'Mark the legacy page' 2

    Invoke-Git $rich @('checkout', '-q', 'main') | Out-Null
    Invoke-Git $rich @('checkout', '-q', '-b', 'feature/login') | Out-Null
    Write-Text $rich 'src/login.txt' (Get-Source 'login' 32)
    New-Commit $rich 'Add the login module' 1
    Write-Text $rich 'src/app.txt' (Get-Source 'app' 40 @{ 5 = '    let value_5 = login(input.user);'; 6 = '    let value_6 = session(value_5);' })
    New-Commit $rich 'Wire the login into the app' 1
    Write-Text $rich 'src/login.txt' (Get-Source 'login' 32 @{ 12 = '    let value_12 = hash(value_11);' })
    New-Commit $rich 'Hash the login secret' 2

    Invoke-Git $rich @('checkout', '-q', 'main') | Out-Null
    Write-Text $rich 'src/app.txt' (Get-Source 'app' 40 @{ 33 = '    let value_33 = compute_fast(33, input.seed);'; 34 = '    let value_34 = compute_fast(34, input.seed);' })
    New-Commit $rich 'Speed up the app module' 0
    Set-Actor 0 -AdvanceClock
    Invoke-Git $rich @('merge', '-q', '--no-ff', 'feature/login', '-m', "Merge branch 'feature/login'") | Out-Null
    Write-Text $rich 'CHANGELOG.md' @('# Changelog', '', '## 0.2.0', '- login', '- faster app module')
    New-Commit $rich 'Add the changelog' 2
    Invoke-Git $rich @('tag', 'v0.2.0') | Out-Null

    # A branch that will conflict with main in docs/guide.md, and the matching commit on main.
    Invoke-Git $rich @('checkout', '-q', '-b', 'docs/rewrite') | Out-Null
    Write-Text $rich 'docs/guide.md' @('# Guide', '', 'Install the tool with the installer.', 'Configure the tool.', 'Run the tool from the menu.', 'Report problems here.')
    New-Commit $rich 'Rewrite the guide for the installer' 1
    Invoke-Git $rich @('checkout', '-q', 'main') | Out-Null
    Write-Text $rich 'docs/guide.md' @('# Guide', '', 'Install the tool from source.', 'Configure the tool.', 'Run the tool on the command line.', 'Report problems here.')
    New-Commit $rich 'Describe the source install in the guide' 0

    # Two stashes (the second one includes an untracked file).
    Write-Text $rich 'src/settings.txt' (Get-Source 'settings' 28 @{ 4 = '    let value_4 = sidebar_width(input);' })
    Set-Actor 1 -AdvanceClock
    Invoke-Git $rich @('stash', 'push', '-q', '-m', 'WIP: sidebar layout') | Out-Null
    Write-Text $rich 'src/settings.txt' (Get-Source 'settings' 28 @{ 7 = '    let value_7 = colour_token(input);' })
    Write-Text $rich 'drafts/ideas.txt' @('colour tokens', 'dark and light variants')
    Set-Actor 2 -AdvanceClock
    Invoke-Git $rich @('stash', 'push', '-q', '-u', '-m', 'experiment: colour tokens') | Out-Null

    # The remote: push everything, let a second clone move main ahead of us, then be ahead ourselves.
    Invoke-Git $rich @('remote', 'add', 'origin', $remote) | Out-Null
    Invoke-Git $rich @('push', '-q', '-u', 'origin', 'main', 'feature/login', 'fix/typo') | Out-Null
    Invoke-Git $rich @('push', '-q', 'origin', '--tags') | Out-Null
    $other = Join-Path $Dest 'other-clone'
    Invoke-Git $repos @('clone', '-q', $remote, $other) | Out-Null
    Write-Text $other 'CONTRIBUTING.md' @('# Contributing', '', 'Open a pull request.')
    New-Commit $other 'Add contributing notes' 1
    Invoke-Git $other @('push', '-q', 'origin', 'main') | Out-Null
    Remove-Item -LiteralPath $other -Recurse -Force
    Write-Text $rich 'docs/notes.md' @('# Notes', '', 'Local work that is not pushed yet.')
    New-Commit $rich 'Add local notes' 0
    Invoke-Git $rich @('fetch', '-q', 'origin') | Out-Null

    # A merge in progress: docs/guide.md conflicts; around it staged, unstaged and untracked changes.
    Set-Actor 0 -AdvanceClock
    Invoke-Git $rich @('merge', 'docs/rewrite') -AllowFailure | Out-Null
    Write-Text $rich 'README.md' @('# Rich', '', 'Branches, tags, stashes, a remote and a merge in progress.', '', 'Staged edit.')
    Write-Text $rich 'src/staged-new.txt' (Get-Source 'staged' 12)
    Invoke-Git $rich @('add', 'README.md', 'src/staged-new.txt') | Out-Null
    Invoke-Git $rich @('mv', 'assets-logo.bin', 'assets-logo-v2.bin') | Out-Null
    Invoke-Git $rich @('rm', '-q', 'docs/legacy.md') | Out-Null
    Write-Text $rich 'src/app.txt' (Get-Source 'app' 40 @{ 20 = '    let value_20 = unstaged_edit(20);'; 21 = '    let value_21 = unstaged_edit(21);' })
    Write-Text $rich 'src/untracked.txt' @('not added yet')
    Write-Text $rich 'notes/todo draft.txt' @('- review the merge', '- resolve docs/guide.md')

    # --- rebase: a rebase of "topic" onto "main" stopped by a conflict (step 1 of 3) ---
    $rebase = New-Repo 'rebase'
    Write-Text $rebase 'src/engine.txt' (Get-Source 'engine' 30)
    Write-Text $rebase 'README.md' @('# Rebase', '', 'A rebase stopped by a conflict.')
    New-Commit $rebase 'Initial commit' 0
    Write-Text $rebase 'src/options.txt' (Get-Source 'options' 20)
    New-Commit $rebase 'Add engine options' 1
    Invoke-Git $rebase @('checkout', '-q', '-b', 'topic') | Out-Null
    Write-Text $rebase 'src/engine.txt' (Get-Source 'engine' 30 @{ 5 = '    let value_5 = topic_value(5);'; 6 = '    let value_6 = topic_value(6);' })
    New-Commit $rebase 'Teach the engine the topic values' 1
    Write-Text $rebase 'src/topic-a.txt' (Get-Source 'topic_a' 14)
    New-Commit $rebase 'Add topic part A' 2
    Write-Text $rebase 'src/topic-b.txt' (Get-Source 'topic_b' 14)
    New-Commit $rebase 'Add topic part B' 2
    Invoke-Git $rebase @('checkout', '-q', 'main') | Out-Null
    Write-Text $rebase 'src/engine.txt' (Get-Source 'engine' 30 @{ 5 = '    let value_5 = main_value(5);'; 6 = '    let value_6 = main_value(6);' })
    New-Commit $rebase 'Change the engine values on main' 0
    Invoke-Git $rebase @('checkout', '-q', 'topic') | Out-Null
    Set-Actor 1 -AdvanceClock
    Invoke-Git $rebase @('rebase', 'main') -AllowFailure | Out-Null
}

# The marker goes in last: a sandbox without it was not finished and the gate refuses it.
[IO.File]::WriteAllText((Join-Path $Dest '.qa-sandbox'), "$(Get-QaMarkerText)`ncreated by tools/qa/new-sandbox.ps1; synthetic data only`n", [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $Dest 'README.txt'), "QA sandbox of GitCthulhu (tools/qa). Synthetic data only. Recreate with new-sandbox.ps1 -Force.`n", [Text.UTF8Encoding]::new($false))

Write-Host "Sandbox ready: $Dest"
foreach ($name in 'linear', 'rich', 'rebase') {
    $path = Join-Path $repos $name
    $state = (Invoke-WithEnvironment -Variables $environment -Script { Invoke-Git $path @('status', '--short', '--branch') | Select-Object -First 1 })
    Write-Host ("  repos\{0,-8} {1}" -f $name, $state)
}
$Dest
