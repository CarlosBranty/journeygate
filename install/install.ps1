# Installs the prebuilt guard binary on Windows from a GitHub release.
#
# Usage (download the script, then run it):
#   powershell -ExecutionPolicy Bypass -File install.ps1
#
# Environment (all optional):
#   GUARD_VERSION      release to install, "0.1.0" or "v0.1.0" (default: latest)
#   GUARD_REPO         owner/name of the repository with the releases
#                      (default: CarlosBranty/journeygate)
#   GUARD_INSTALL_DIR  where guard.exe goes (default: %LOCALAPPDATA%\guard\bin)
#   GITHUB_TOKEN or GH_TOKEN
#                      sent as an Authorization header when set. Needed for a
#                      private repository; then the assets come from the REST API.
#
# Release assets: guard-<target>.tar.gz (guard.exe at the archive root) and
# guard-<target>.tar.gz.sha256 ("<hex>  <asset>"), built by
# .github/workflows/release.yml. The checksum is verified before extracting.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Fail([string] $message) {
    Write-Error "error: $message" -ErrorAction Continue
    exit 1
}

$repo = if ($env:GUARD_REPO) { $env:GUARD_REPO } else { 'CarlosBranty/journeygate' }
$version = if ($env:GUARD_VERSION) { $env:GUARD_VERSION } else { 'latest' }
$version = $version -replace '^v', ''
$installDir = if ($env:GUARD_INSTALL_DIR) { $env:GUARD_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'guard\bin' }
$token = if ($env:GITHUB_TOKEN) { $env:GITHUB_TOKEN } elseif ($env:GH_TOKEN) { $env:GH_TOKEN } else { '' }

if ((Test-Path variable:IsWindows) -and -not $IsWindows) {
    Fail 'install.ps1 installs the Windows binary; on Linux and macOS use install.sh'
}
switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' {
        # Windows on Arm runs the x64 binary through emulation.
        Write-Host 'note: no ARM64 build is published; installing the x64 binary'
        $target = 'x86_64-pc-windows-msvc'
    }
    default { Fail "unsupported Windows architecture: $($env:PROCESSOR_ARCHITECTURE)" }
}
if (-not (Get-Command tar -ErrorAction SilentlyContinue)) {
    Fail 'tar.exe is required (included in Windows 10 1803 and later)'
}

$asset = "guard-$target.tar.gz"
$label = if ($version -eq 'latest') { 'the latest release' } else { "v$version" }
Write-Host "installing guard $label ($target) from $repo"

$work = Join-Path ([IO.Path]::GetTempPath()) ("guard-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null

# Both Windows PowerShell and PowerShell 7 drop the Authorization header on the
# redirect to the storage host, so the token never leaves github.com.
function Get-Headers([string] $accept) {
    $headers = @{ 'Accept' = $accept }
    if ($token) {
        $headers['Authorization'] = "Bearer $token"
        $headers['X-GitHub-Api-Version'] = '2022-11-28'
    }
    return $headers
}

try {
    $archive = Join-Path $work $asset
    $checksum = Join-Path $work "$asset.sha256"
    if ($token) {
        # Private repositories: github.com download links need a browser
        # session, so resolve the assets through the REST API.
        $api = if ($version -eq 'latest') {
            "https://api.github.com/repos/$repo/releases/latest"
        } else {
            "https://api.github.com/repos/$repo/releases/tags/v$version"
        }
        try {
            $release = Invoke-RestMethod -UserAgent 'guard-installer' -Uri $api -Headers (Get-Headers 'application/vnd.github+json')
        } catch {
            Fail "cannot read $label of $repo (check the token's access)"
        }
        foreach ($pair in @(@($asset, $archive), @("$asset.sha256", $checksum))) {
            $found = $release.assets | Where-Object { $_.name -eq $pair[0] } | Select-Object -First 1
            if (-not $found) { Fail "$label of $repo has no asset $($pair[0])" }
            Invoke-WebRequest -UseBasicParsing -UserAgent 'guard-installer' -Uri $found.url -Headers (Get-Headers 'application/octet-stream') -OutFile $pair[1]
        }
    } else {
        $base = if ($version -eq 'latest') {
            "https://github.com/$repo/releases/latest/download"
        } else {
            "https://github.com/$repo/releases/download/v$version"
        }
        foreach ($pair in @(@($asset, $archive), @("$asset.sha256", $checksum))) {
            try {
                Invoke-WebRequest -UseBasicParsing -UserAgent 'guard-installer' -Uri "$base/$($pair[0])" -Headers (Get-Headers '*/*') -OutFile $pair[1]
            } catch {
                Fail "cannot download $base/$($pair[0]) (a private repository needs GITHUB_TOKEN or GH_TOKEN)"
            }
        }
    }

    # Verify, then extract: never unpack an archive whose checksum differs.
    $expected = ((Get-Content -Raw $checksum).Trim() -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -Path $archive).Hash.ToLowerInvariant()
    if ($expected -ne $actual) { Fail "checksum mismatch for $asset" }

    $out = Join-Path $work 'out'
    New-Item -ItemType Directory -Path $out | Out-Null
    tar -xzf $archive -C $out
    if ($LASTEXITCODE -ne 0) { Fail "cannot extract $asset" }
    $exe = Join-Path $out 'guard.exe'
    if (-not (Test-Path $exe)) { Fail "$asset does not contain guard.exe" }

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Copy-Item -Force $exe (Join-Path $installDir 'guard.exe')
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

$installed = Join-Path $installDir 'guard.exe'
Write-Host "installed $installed"
& $installed --version

$onPath = ($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $installDir.TrimEnd('\') }
if (-not $onPath) {
    Write-Host ''
    Write-Host "$installDir is not on your PATH. Add it for your user with:"
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$installDir', 'User')"
    Write-Host 'then open a new terminal.'
}
