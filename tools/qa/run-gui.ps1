<#
.SYNOPSIS
  Runs one QA scenario against a sandbox in the qa build of GitCthulhu and returns the screenshot paths.

.DESCRIPTION
  Builds the app with `--features qa` (an incremental no-op when nothing changed), resets the sandbox's app data
  (so every run starts from the same state; -KeepState keeps it), starts the app with
  `--qa-scenario <file> --qa-out <dir>` and waits for it. The app drives itself in-process (no OS input is
  injected by this script or by the app), captures its own window with PrintWindow into PNG files, writes
  report.txt / report.json / verbs.txt into the output directory and exits. The process is killed (exit code 124)
  when it does not finish within -TimeoutSec; only the process started here is ever stopped.

  The app window appears on the desktop for the length of the run (it does not take the keyboard focus).
  Keep runs short and few.

  Output (pipeline): the screenshot paths, one per line. The script's exit code is the app's: 0 = all steps
  passed, 1 = a step failed, 2 = usage error, 3 = the sandbox gate refused, 124 = timeout, 101 = panic
  (see panic.txt), 125 = no report was written.

.PARAMETER Sandbox
  A sandbox made by new-sandbox.ps1.

.PARAMETER Scenario
  The scenario file (see tools\qa\scenarios and the module documentation of crates\gitcthulhu\src\qa\verbs.rs).

.PARAMETER Out
  Output directory (default <Sandbox>\out\<scenario name>). Old reports and screenshots in it are removed first.

.PARAMETER Exe
  The executable to run (default target\debug\gitcthulhu.exe). It must be a qa build: a plain build would ignore
  the --qa-* flags, so the script refuses it.

.PARAMETER NoBuild
  Skip the cargo build (use the existing executable).

.PARAMETER KeepState
  Do not reset the sandbox's app data before the run.

.PARAMETER HttpServer
  Starts the local credential-checking Git test server (tools\qa\git-http-server.ps1) on a free loopback port for
  the run: repos\cred-remote.git is served over http, the "origin" of repos\cred points at it, and the qa gate lets
  the http protocol through for that port only (no real network). The user name is qa-user and the password
  qa-secret (synthetic). The remote and the clone's tracking ref are reset to "one commit behind" first, so every
  run pushes again. The server's request log is copied to <Out>\http-server.txt. Needs a sandbox with repos\cred.

.PARAMETER TimeoutSec
  Wall-clock limit of the run: the app's own watchdog ends it then (writing a partial report); this script kills
  the process 15 s later if it is still running.

.EXAMPLE
  pwsh -NoProfile -File tools\qa\run-gui.ps1 -Sandbox $sbx -Scenario tools\qa\scenarios\welcome.qa
#>
param(
    [Parameter(Mandatory = $true)][string]$Sandbox,
    [Parameter(Mandatory = $true)][string]$Scenario,
    [string]$Out,
    [string]$Exe,
    [switch]$NoBuild,
    [switch]$KeepState,
    [switch]$HttpServer,
    [int]$TimeoutSec = 90
)
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'qa-common.psm1') -Force

$repoRoot = Get-QaRepoRoot
$Sandbox = [System.IO.Path]::GetFullPath($Sandbox).TrimEnd('\', '/')
if (-not (Test-QaSandbox $Sandbox)) {
    throw "'$Sandbox' is not a QA sandbox (missing marker). Create one with tools\qa\new-sandbox.ps1."
}
$Scenario = (Resolve-Path -LiteralPath $Scenario).Path
if (-not $Out) { $Out = Join-Path $Sandbox ('out\' + [IO.Path]::GetFileNameWithoutExtension($Scenario)) }
$Out = [System.IO.Path]::GetFullPath($Out)

if (-not $NoBuild) {
    Write-Host 'cargo build -p gitcthulhu --features qa'
    Push-Location $repoRoot
    try {
        & cargo build -p gitcthulhu --features qa
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
    }
    finally { Pop-Location }
}
if (-not $Exe) { $Exe = Join-Path $repoRoot 'target\debug\gitcthulhu.exe' }
if (-not (Test-Path -LiteralPath $Exe)) { throw "Executable not found: $Exe (build with --features qa)" }
# A plain build (for example a later `cargo build -p gitcthulhu` in this worktree) has no gate and ignores the
# --qa-* flags: it would open a window that nothing ends. Only a qa build contains the marker text of the gate.
$exeText = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Exe).Path))
if (-not $exeText.Contains((Get-QaMarkerText))) {
    throw "$Exe is not a qa build (rebuild with: cargo build -p gitcthulhu --features qa, or drop -NoBuild)"
}
Remove-Variable exeText

# A fresh app state per run: settings, UI state (open tabs, window), repository preferences, logs.
if (-not $KeepState) {
    foreach ($sub in 'app', 'profile\AppData\Roaming', 'profile\AppData\Local') {
        $dir = Join-Path $Sandbox $sub
        if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
}

# Old results must not be mistaken for this run's.
New-Item -ItemType Directory -Force -Path $Out | Out-Null
Get-ChildItem -LiteralPath $Out -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Extension -in '.png', '.txt', '.json' } | Remove-Item -Force

$stdout = Join-Path $Out 'stdout.txt'
$stderr = Join-Path $Out 'stderr.txt'
$httpPort = 0
$serverProcess = $null
$serverDir = Join-Path $Sandbox 'http'
$stopFile = Join-Path $serverDir 'stop'
$serverLog = Join-Path $serverDir 'requests.log'
if ($HttpServer) {
    $credRepo = Join-Path $Sandbox 'repos\cred'
    $credRemote = Join-Path $Sandbox 'repos\cred-remote.git'
    if (-not ((Test-Path -LiteralPath $credRepo) -and (Test-Path -LiteralPath $credRemote))) {
        throw "the sandbox has no repos\cred / repos\cred-remote.git: recreate it with new-sandbox.ps1 -Force"
    }
    $probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $probe.Start()
    $httpPort = $probe.LocalEndpoint.Port
    $probe.Stop()
    if (Test-Path -LiteralPath $serverDir) { Remove-Item -LiteralPath $serverDir -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $serverDir | Out-Null
    $httpEnvironment = Get-QaEnvironment -Sandbox $Sandbox -HttpPort $httpPort
    Invoke-WithEnvironment -Variables $httpEnvironment -Script {
        $url = "http://127.0.0.1:$httpPort/cred-remote.git"
        & git -C $credRepo remote set-url origin $url
        # Back to "one commit behind": the remote and the clone's tracking ref go to the parent of HEAD.
        $parent = (& git -C $credRepo rev-parse 'HEAD~1').Trim()
        & git -C $credRemote update-ref refs/heads/main $parent
        & git -C $credRepo update-ref refs/remotes/origin/main $parent
    }
    $readyFile = Join-Path $serverDir 'ready'
    $serverArgs = @(
        '-NoProfile', '-File', (Join-Path $PSScriptRoot 'git-http-server.ps1'),
        '-Root', (Join-Path $Sandbox 'repos'), '-Port', [string]$httpPort,
        '-User', 'qa-user', '-Password', 'qa-secret', '-Log', $serverLog,
        '-ReadyFile', $readyFile, '-StopFile', $stopFile, '-TimeoutSec', [string]($TimeoutSec + 30)
    )
    $serverProcess = Invoke-WithEnvironment -Variables $httpEnvironment -Script {
        Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList $serverArgs -PassThru -WindowStyle Hidden
    }
    $ready = $false
    for ($i = 0; $i -lt 150 -and -not $ready; $i++) {
        if (Test-Path -LiteralPath $readyFile) { $ready = $true } else { Start-Sleep -Milliseconds 100 }
        if ($serverProcess.HasExited) { break }
    }
    if (-not $ready) {
        if (-not $serverProcess.HasExited) { Stop-Process -Id $serverProcess.Id -Force }
        throw "the Git test server did not start (see $serverLog)"
    }
    Write-Host "git http server: 127.0.0.1:$httpPort (user qa-user)"
}
$environment = Get-QaEnvironment -Sandbox $Sandbox -HttpPort $httpPort
$environment['RUST_BACKTRACE'] = '1'
$arguments = @('--qa-scenario', "`"$Scenario`"", '--qa-out', "`"$Out`"", '--qa-timeout', [string]$TimeoutSec)
$killAfterSec = $TimeoutSec + 15

Write-Host "scenario: $Scenario"
Write-Host "out:      $Out"
try {
    $exitCode = Invoke-WithEnvironment -Variables $environment -Script {
        $process = Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru -NoNewWindow `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr -WorkingDirectory $Sandbox
        $null = $process.Handle   # keeps the handle so ExitCode is readable after WaitForExit
        if (-not $process.WaitForExit($killAfterSec * 1000)) {
            Write-Warning "no exit after $killAfterSec s: stopping process $($process.Id) (started by this script)"
            Stop-Process -Id $process.Id -Force
            $process.WaitForExit(5000) | Out-Null
            124
        }
        else { $process.ExitCode }
    }
}
finally {
    if ($serverProcess) {
        # Only the server this script started is ever stopped: ask it to end, then stop it if it does not.
        Set-Content -LiteralPath $stopFile -Value 'stop' -Encoding ascii
        if (-not $serverProcess.WaitForExit(5000)) { Stop-Process -Id $serverProcess.Id -Force }
        if (Test-Path -LiteralPath $serverLog) { Copy-Item -LiteralPath $serverLog -Destination (Join-Path $Out 'http-server.txt') -Force }
    }
}

$reportPath = Join-Path $Out 'report.txt'
if (Test-Path -LiteralPath $reportPath) {
    Get-Content -LiteralPath $reportPath | ForEach-Object { Write-Host $_ }
}
else {
    Write-Warning 'no report.txt was written (the app crashed, was refused or timed out before the first step)'
    if (Test-Path -LiteralPath $stderr) { Get-Content -LiteralPath $stderr -TotalCount 20 | ForEach-Object { Write-Host "stderr: $_" } }
    if ($exitCode -eq 0) { $exitCode = 125 }
}
$panic = Join-Path $Out 'panic.txt'
if (Test-Path -LiteralPath $panic) { Write-Warning ('panic: ' + (Get-Content -LiteralPath $panic -TotalCount 3 -Raw)) }
Write-Host "exit code: $exitCode"

Get-ChildItem -LiteralPath $Out -Filter '*.png' -File | Sort-Object Name | ForEach-Object { $_.FullName }
exit $exitCode
