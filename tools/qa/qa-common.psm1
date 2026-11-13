# Shared helpers of the QA tools (new-sandbox.ps1, run-gui.ps1).
#
# The sandbox gate of the qa build (crates/gitcthulhu/src/qa/gate.rs) checks the process environment this module
# produces: every data location inside the sandbox, Git isolated from the user's configuration, no network protocol.

Set-StrictMode -Version Latest

$script:MarkerFile = '.qa-sandbox'
$script:MarkerText = 'gitcthulhu-qa-sandbox v1'

function Get-QaMarkerFile { $script:MarkerFile }
function Get-QaMarkerText { $script:MarkerText }

function Get-QaRepoRoot {
    # tools/qa -> repository root
    (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
}

function Test-QaSandbox {
    param([Parameter(Mandatory)][string]$Path)
    $marker = Join-Path $Path $script:MarkerFile
    if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) { return $false }
    (Get-Content -LiteralPath $marker -TotalCount 1) -eq $script:MarkerText
}

function Get-QaEnvironment {
    <#
    .SYNOPSIS
      The environment variables a process needs to run against the sandbox.
    #>
    param(
        [Parameter(Mandatory)][string]$Sandbox,
        # The loopback port of the local Git test server (credential scenario): allows the http protocol for it.
        [int]$HttpPort = 0
    )
    $profileDir = Join-Path $Sandbox 'profile'
    $environment = [ordered]@{
        GITCTHULHU_QA_SANDBOX = $Sandbox
        GITCTHULHU_HOME       = Join-Path $Sandbox 'app'
        USERPROFILE           = $profileDir
        HOME                  = $profileDir
        APPDATA               = Join-Path $profileDir 'AppData\Roaming'
        LOCALAPPDATA          = Join-Path $profileDir 'AppData\Local'
        XDG_CONFIG_HOME       = Join-Path $profileDir '.config'
        XDG_DATA_HOME         = Join-Path $profileDir '.local\share'
        XDG_CACHE_HOME        = Join-Path $profileDir '.cache'
        GIT_CONFIG_GLOBAL     = Join-Path $Sandbox 'gitconfig'
        GIT_CONFIG_NOSYSTEM   = '1'
        GIT_ALLOW_PROTOCOL    = 'file'
        GIT_TERMINAL_PROMPT   = '0'
    }
    if ($HttpPort -gt 0) {
        # Only the gate's pairing passes (qa/gate.rs): http for the loopback test server, nothing else. The
        # loopback server is exempt from proxies; every other host goes to a proxy on the discard port of this
        # machine, which nothing answers, so a fixture or scenario that names another http host cannot reach the
        # network while http is allowed.
        $environment['GIT_ALLOW_PROTOCOL'] = 'file:http'
        $environment['GITCTHULHU_QA_LOOPBACK_HTTP'] = "127.0.0.1:$HttpPort"
        $environment['NO_PROXY'] = '127.0.0.1'
        $environment['no_proxy'] = '127.0.0.1'
        foreach ($name in 'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy') {
            $environment[$name] = 'http://127.0.0.1:9'
        }
    }
    $environment
}

function Invoke-WithEnvironment {
    <#
    .SYNOPSIS
      Runs a script block with extra environment variables and restores the previous values afterwards.
    #>
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary]$Variables,
        [Parameter(Mandatory)][scriptblock]$Script
    )
    $saved = @{}
    foreach ($name in $Variables.Keys) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, [string]$Variables[$name], 'Process')
    }
    try { & $Script }
    finally {
        foreach ($name in $saved.Keys) {
            [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process')
        }
    }
}

function Assert-UnderScratch {
    <#
    .SYNOPSIS
      Refuses a sandbox location that is not below a folder named "scratch" or the system temp folder.
    #>
    param([Parameter(Mandatory)][string]$Path)
    $full = [System.IO.Path]::GetFullPath($Path).TrimEnd('\', '/')
    $segments = $full -split '[\\/]'
    $temp = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\', '/')
    $inTemp = $full.StartsWith($temp + [System.IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)
    $inScratch = $segments[0..([Math]::Max(0, $segments.Count - 2))] | Where-Object { $_ -ieq 'scratch' }
    if (-not ($inTemp -or $inScratch)) {
        throw "Refusing '$full': a QA sandbox must live below a folder named 'scratch' or below the temp folder ($temp)."
    }
    $full
}
