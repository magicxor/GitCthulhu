<#
.SYNOPSIS
  Proves the credential flow end to end in the real app window: askpass -> credential dialog -> reply -> push.

.DESCRIPTION
  Runs tools\qa\scenarios\m2-credentials.qa through run-gui.ps1 -HttpServer (a local Git smart-HTTP server on a
  loopback port that requires Basic credentials; no real network), then checks the outcome with git:
    - the bare remote's main is now the clone's main (the push arrived),
    - the server saw an unauthenticated request answered 401 first and authenticated requests after the dialogs
      (the log has user names, never the password),
    - the app's own output (stdout, stderr, the logs of the run; not the runner's quote of the typed step) and the
      server log do not contain the password.
  Exit code: 0 everything holds, 1 a check failed, otherwise the scenario's exit code.

.PARAMETER Sandbox
  A sandbox made by new-sandbox.ps1 (with repos\cred).

.PARAMETER Out
  Output directory (default <Sandbox>\out\m2-credentials).

.PARAMETER NoBuild
  Skip the cargo build.

.EXAMPLE
  pwsh -NoProfile -File tools\qa\run-credential-flow.ps1 -Sandbox C:\git\opensource\GitCthulhu.plan\scratch\fix1\sbx
#>
param(
    [Parameter(Mandatory = $true)][string]$Sandbox,
    [string]$Out,
    [switch]$NoBuild
)
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'qa-common.psm1') -Force

$Sandbox = [System.IO.Path]::GetFullPath($Sandbox).TrimEnd('\', '/')
if (-not $Out) { $Out = Join-Path $Sandbox 'out\m2-credentials' }
$runArgs = @{
    Sandbox    = $Sandbox
    Scenario   = (Join-Path $PSScriptRoot 'scenarios\m2-credentials.qa')
    Out        = $Out
    HttpServer = $true
}
if ($NoBuild) { $runArgs['NoBuild'] = $true }
& (Join-Path $PSScriptRoot 'run-gui.ps1') @runArgs
$scenarioExit = $LASTEXITCODE
if ($scenarioExit -ne 0) { Write-Host "scenario exit code $scenarioExit"; exit $scenarioExit }

$failures = [System.Collections.Generic.List[string]]::new()
$environment = Get-QaEnvironment -Sandbox $Sandbox
$git = (Get-Command git -ErrorAction Stop).Source
$heads = Invoke-WithEnvironment -Variables $environment -Script {
    @{
        local  = (& $git -C (Join-Path $Sandbox 'repos\cred') rev-parse 'refs/heads/main').Trim()
        remote = (& $git -C (Join-Path $Sandbox 'repos\cred-remote.git') rev-parse 'refs/heads/main').Trim()
    }
}
if ($heads.local -ne $heads.remote) { $failures.Add("the push did not arrive: local main $($heads.local), remote main $($heads.remote)") }

$serverLog = Join-Path $Out 'http-server.txt'
$requests = if (Test-Path -LiteralPath $serverLog) { Get-Content -LiteralPath $serverLog } else { @() }
$first401 = [Array]::FindIndex([string[]]$requests, [Predicate[string]] { param($l) $l -match ' 401 user=-$' })
$firstOk = [Array]::FindIndex([string[]]$requests, [Predicate[string]] { param($l) $l -match ' 200 user=qa-user$' })
if ($first401 -lt 0) { $failures.Add('the server never answered an unauthenticated request with 401') }
if ($firstOk -lt 0) { $failures.Add('the server never saw authenticated requests') }
elseif ($first401 -ge 0 -and $firstOk -lt $first401) { $failures.Add('authenticated requests came before the 401') }
if (-not ($requests -match 'git-receive-pack')) { $failures.Add('no git-receive-pack request in the server log') }

# report.txt and the runner's own step log quote the scenario line `type qa-secret`: that is the test input, not a leak.
foreach ($file in 'stdout.txt', 'stderr.txt', 'http-server.txt') {
    $path = Join-Path $Out $file
    if ((Test-Path -LiteralPath $path) -and (Select-String -LiteralPath $path -Pattern 'qa-secret' -SimpleMatch -Quiet)) {
        $failures.Add("$file contains the password")
    }
}
$appLogs = Join-Path $Sandbox 'app'
if (Test-Path -LiteralPath $appLogs) {
    $leak = Get-ChildItem -LiteralPath $appLogs -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object {
            Select-String -LiteralPath $_.FullName -Pattern 'qa-secret' -SimpleMatch |
                Where-Object { $_.Line -notmatch 'gitcthulhu::qa::runner' }
        }
    foreach ($file in $leak) { $failures.Add("the app data file $($file.FullName) contains the password") }
}

Write-Host "remote main: $($heads.remote)  local main: $($heads.local)"
$requests | ForEach-Object { Write-Host "server: $_" }
if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Warning $_ }
    exit 1
}
Write-Host 'credential flow: askpass -> dialog -> reply -> push verified'
exit 0
