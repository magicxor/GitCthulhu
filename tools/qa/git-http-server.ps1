<#
.SYNOPSIS
  A local smart-HTTP Git server that requires HTTP Basic credentials (QA credential scenario; no real network).

.DESCRIPTION
  Serves the bare repositories below -Root over `http://127.0.0.1:<Port>/<name>.git` through `git http-backend`
  (CGI). Every request needs `Authorization: Basic` for -User / -Password; without it the server answers 401, which
  makes Git ask for the credentials (through the app's askpass helper, then the credential dialog). Pushes are
  enabled for the authenticated user.

  The server listens on the loopback interface only, ends after -TimeoutSec at the latest, and ends when the file
  named by -StopFile appears. It writes one line per request to -Log (method, path, status, user name; never the
  password) and the line `ready` to -ReadyFile once it listens.

  Started and stopped by tools\qa\run-gui.ps1 -HttpServer; it can also be run by hand for experiments.

.PARAMETER Root
  Directory that holds the bare repositories (GIT_PROJECT_ROOT).

.PARAMETER Port
  The loopback port to listen on.

.PARAMETER User
  The user name the server accepts.

.PARAMETER Password
  The password the server accepts (a synthetic test value; it is never logged).

.PARAMETER Log
  Request log file.

.PARAMETER ReadyFile
  Written when the server listens.

.PARAMETER StopFile
  The server ends when this file exists.

.PARAMETER TimeoutSec
  Hard limit of the server's lifetime.
#>
param(
    [Parameter(Mandatory = $true)][string]$Root,
    [Parameter(Mandatory = $true)][int]$Port,
    [Parameter(Mandatory = $true)][string]$User,
    [Parameter(Mandatory = $true)][string]$Password,
    [Parameter(Mandatory = $true)][string]$Log,
    [string]$ReadyFile,
    [string]$StopFile,
    [int]$TimeoutSec = 180
)
$ErrorActionPreference = 'Stop'

$Root = [IO.Path]::GetFullPath($Root)
$git = (Get-Command git -ErrorAction Stop).Source
$execPath = (& $git --exec-path).Trim()
$backend = Join-Path $execPath 'git-http-backend.exe'
if (-not (Test-Path -LiteralPath $backend)) { $backend = Join-Path $execPath 'git-http-backend' }
if (-not (Test-Path -LiteralPath $backend)) { throw "git-http-backend not found in $execPath" }

$expected = 'Basic ' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("${User}:${Password}"))

function Write-Log([string]$Line) {
    Add-Content -LiteralPath $Log -Value $Line -Encoding utf8
}

function Send-Text($Response, [int]$Status, [string]$Text) {
    $bytes = [Text.Encoding]::UTF8.GetBytes($Text)
    $Response.StatusCode = $Status
    $Response.ContentType = 'text/plain'
    $Response.ContentLength64 = $bytes.Length
    $Response.OutputStream.Write($bytes, 0, $bytes.Length)
    $Response.Close()
}

function Invoke-Backend($Request, $Response, [string]$RemoteUser) {
    $body = [IO.MemoryStream]::new()
    if ($Request.HasEntityBody) { $Request.InputStream.CopyTo($body) }
    $info = [Diagnostics.ProcessStartInfo]::new($backend)
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.CreateNoWindow = $true
    $info.Environment['GIT_PROJECT_ROOT'] = $Root
    $info.Environment['GIT_HTTP_EXPORT_ALL'] = '1'
    $info.Environment['REQUEST_METHOD'] = $Request.HttpMethod
    $info.Environment['PATH_INFO'] = [Uri]::UnescapeDataString($Request.Url.AbsolutePath)
    $info.Environment['QUERY_STRING'] = $Request.Url.Query.TrimStart('?')
    $info.Environment['REMOTE_USER'] = $RemoteUser
    $info.Environment['REMOTE_ADDR'] = '127.0.0.1'
    $info.Environment['CONTENT_LENGTH'] = [string]$body.Length
    if ($Request.ContentType) { $info.Environment['CONTENT_TYPE'] = $Request.ContentType }
    $encoding = $Request.Headers['Content-Encoding']
    if ($encoding) { $info.Environment['HTTP_CONTENT_ENCODING'] = $encoding }
    $protocol = $Request.Headers['Git-Protocol']
    if ($protocol) { $info.Environment['GIT_PROTOCOL'] = $protocol }
    $process = [Diagnostics.Process]::Start($info)
    $errors = $process.StandardError.ReadToEndAsync()
    $output = [IO.MemoryStream]::new()
    $copy = $process.StandardOutput.BaseStream.CopyToAsync($output)
    $body.Position = 0
    $body.CopyTo($process.StandardInput.BaseStream)
    $process.StandardInput.BaseStream.Close()
    $copy.Wait()
    $process.WaitForExit()
    $bytes = $output.ToArray()

    # CGI response: headers, a blank line, the body.
    $split = -1
    $skip = 4
    for ($i = 0; $i -lt $bytes.Length - 1; $i++) {
        if ($i + 3 -lt $bytes.Length -and $bytes[$i] -eq 13 -and $bytes[$i + 1] -eq 10 -and $bytes[$i + 2] -eq 13 -and $bytes[$i + 3] -eq 10) { $split = $i; $skip = 4; break }
        if ($bytes[$i] -eq 10 -and $bytes[$i + 1] -eq 10) { $split = $i; $skip = 2; break }
    }
    if ($split -lt 0) {
        Write-Log "backend produced no CGI headers: $($errors.Result)"
        return 500
    }
    $head = [Text.Encoding]::ASCII.GetString($bytes, 0, $split)
    $status = 200
    foreach ($line in ($head -split "`r?`n")) {
        $colon = $line.IndexOf(':')
        if ($colon -lt 1) { continue }
        $name = $line.Substring(0, $colon)
        $value = $line.Substring($colon + 1).Trim()
        switch -Regex ($name) {
            '^Status$' { $status = [int]($value -split ' ')[0] }
            '^Content-Type$' { $Response.ContentType = $value }
            default { $Response.Headers[$name] = $value }
        }
    }
    $start = $split + $skip
    $Response.StatusCode = $status
    $Response.ContentLength64 = $bytes.Length - $start
    if ($bytes.Length -gt $start) { $Response.OutputStream.Write($bytes, $start, $bytes.Length - $start) }
    $Response.Close()
    return $status
}

$listener = [Net.HttpListener]::new()
$listener.Prefixes.Add("http://127.0.0.1:$Port/")
$listener.Start()
Write-Log "listening on 127.0.0.1:$Port, root $Root"
if ($ReadyFile) { Set-Content -LiteralPath $ReadyFile -Value 'ready' -Encoding ascii }

$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSec)
try {
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($StopFile -and (Test-Path -LiteralPath $StopFile)) { break }
        $pending = $listener.GetContextAsync()
        while (-not $pending.Wait(200)) {
            if ([DateTime]::UtcNow -ge $deadline -or ($StopFile -and (Test-Path -LiteralPath $StopFile))) { break }
        }
        if (-not $pending.IsCompleted) { break }
        $context = $pending.Result
        $request = $context.Request
        $response = $context.Response
        $path = $request.Url.AbsolutePath
        $method = $request.HttpMethod
        try {
            $header = $request.Headers['Authorization']
            if ($header -ne $expected) {
                $seen = '-'
                if ($header -like 'Basic *') {
                    try {
                        $decoded = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($header.Substring(6)))
                        $seen = ($decoded -split ':', 2)[0]
                    }
                    catch { $seen = '?' }
                }
                $response.Headers['WWW-Authenticate'] = 'Basic realm="gitcthulhu-qa"'
                Send-Text $response 401 'authentication required'
                Write-Log "$method $path 401 user=$seen"
                continue
            }
            $status = Invoke-Backend $request $response $User
            Write-Log "$method $path $status user=$User"
        }
        catch {
            Write-Log "$method $path failed: $($_.Exception.Message)"
            try { Send-Text $response 500 'server error' } catch { }
        }
    }
}
finally {
    $listener.Stop()
    $listener.Close()
    Write-Log 'stopped'
}
