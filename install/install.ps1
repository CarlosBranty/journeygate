# Installs guard on Windows: the prebuilt binary from a GitHub release (or a
# cargo build from source), then checks the tools a guard profile needs.
#
# Usage, from the published short URL:
#   irm <installer url> | iex
# or download the script, then run it:
#   powershell -ExecutionPolicy Bypass -File install.ps1
#
# Options come from the environment only (iex takes no arguments):
#   GUARD_PROFILE      cli | qa | factory (interactive: asked, default qa;
#                      otherwise cli)
#   GUARD_YES=1        no questions; accept installs that need no admin rights
#   GUARD_ALLOW_SUDO=1 allow installs that need admin rights without a question
#   GUARD_DRY_RUN=1    print the plan only: no network, no installs, no writes
#   GUARD_FROM_SOURCE=1 build with cargo instead of downloading a release
#   GUARD_NO_DEPS=1    the binary only (profile cli)
#   GUARD_VERSION      release to install, "0.1.0" or "v0.1.0" (default: latest)
#   GUARD_REPO         owner/name of the repository with the releases
#                      (default: CarlosBranty/journeygate)
#   GUARD_INSTALL_DIR  where guard.exe goes (default: %LOCALAPPDATA%\guard\bin)
#   GITHUB_TOKEN or GH_TOKEN
#                      sent as an Authorization header when set. Needed for a
#                      private repository; then the assets come from the REST API.
#   GUARD_ASSET_FILE   a local guard-<target>.tar.gz to install (offline)
#   GUARD_ASSET_SHA256 its expected sha256 (default: ${GUARD_ASSET_FILE}.sha256)
#   NO_COLOR           disable colors
#   GUARD_OS, GUARD_ARCH
#                      testing only: with GUARD_DRY_RUN=1, GUARD_OS lets a
#                      dry run work on pwsh outside Windows
#
# Release assets: guard-<target>.tar.gz (guard.exe at the archive root) and
# guard-<target>.tar.gz.sha256 ("<hex>  <asset>"), built by
# .github/workflows/release.yml. The checksum is verified before extracting.
#
# Under `irm | iex` the script runs inside your PowerShell session, so it never
# calls `exit` (that would close the window): errors are thrown and reported.

function Install-Guard([string] $token) {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    function Fail([string] $message) {
        throw "error: $message"
    }

    function Test-True([string] $value) {
        return @('1', 'true', 'yes') -contains "$value".ToLowerInvariant()
    }

    $repo = if ($env:GUARD_REPO) { $env:GUARD_REPO } else { 'CarlosBranty/journeygate' }
    $version = if ($env:GUARD_VERSION) { $env:GUARD_VERSION } else { 'latest' }
    $version = $version -replace '^v', ''
    $yes = Test-True $env:GUARD_YES
    $allowAdmin = Test-True $env:GUARD_ALLOW_SUDO
    $dryRun = Test-True $env:GUARD_DRY_RUN
    $fromSource = Test-True $env:GUARD_FROM_SOURCE
    $profileName = if (Test-True $env:GUARD_NO_DEPS) { 'cli' } else { "$env:GUARD_PROFILE" }
    if (@('', 'cli', 'qa', 'factory') -notcontains $profileName) {
        Fail "unknown GUARD_PROFILE: $profileName (cli, qa or factory)"
    }

    $onWindows = -not ((Test-Path variable:IsWindows) -and -not $IsWindows)
    if (-not $onWindows -and -not ($dryRun -and $env:GUARD_OS)) {
        Fail 'install.ps1 installs the Windows binary; on Linux and macOS use install.sh'
    }
    $localAppData = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { [IO.Path]::GetTempPath() }
    $installDir = if ($env:GUARD_INSTALL_DIR) { $env:GUARD_INSTALL_DIR } else { Join-Path $localAppData 'guard\bin' }

    # npm.ps1 may be blocked by the execution policy; the .cmd shims are not.
    $npmExe = if ($onWindows) { 'npm.cmd' } else { 'npm' }
    $npxExe = if ($onWindows) { 'npx.cmd' } else { 'npx' }

    $interactive = [Environment]::UserInteractive -and -not [Console]::IsInputRedirected -and -not $yes -and -not $env:CI
    $color = -not $env:NO_COLOR -and -not [Console]::IsOutputRedirected

    # Shared, mutable state for the nested functions below.
    $state = @{ Rows = New-Object System.Collections.ArrayList; Skipped = New-Object System.Collections.ArrayList; Result = '' }

    function Write-Line([string] $text, [string] $fg) {
        if ($color -and $fg) { Write-Host $text -ForegroundColor $fg } else { Write-Host $text }
    }

    function Get-Mark([string] $mark) {
        $marks = @{ ok = 'Green'; warn = 'Yellow'; fail = 'Red'; skip = 'DarkGray'; plan = 'Cyan' }
        return @{ Text = $mark; Color = $marks[$mark] }
    }

    function Write-Step([int] $n, [string] $label, [string] $mark, [string] $detail) {
        $dots = '.' * [Math]::Max(2, 24 - $label.Length)
        $m = Get-Mark $mark
        if ($color) {
            Write-Host -NoNewline "[$n/5] $label $dots "
            Write-Host -NoNewline $m.Text -ForegroundColor $m.Color
            Write-Host "  $detail"
        } else {
            Write-Host "[$n/5] $label $dots $($m.Text)  $detail"
        }
    }

    function Write-Sub([string] $mark, [string] $text) {
        $m = Get-Mark $mark
        if ($color) {
            Write-Host -NoNewline '      '
            Write-Host -NoNewline $m.Text -ForegroundColor $m.Color
            Write-Host "  $text"
        } else {
            Write-Host "      $($m.Text)  $text"
        }
    }

    function Write-Note([string] $text) { Write-Host "      $text" }

    function Read-YesNo([string] $question, [bool] $default) {
        if (-not $interactive) { return $default }
        $hint = if ($default) { '[Y/n]' } else { '[y/N]' }
        $answer = Read-Host "      $question $hint"
        if ($answer -match '^(y|yes)$') { return $true }
        if ($answer -match '^(n|no)$') { return $false }
        return $default
    }

    function Add-Row([string] $dep, [string] $status, [string] $found) {
        [void] $state.Rows.Add(('  {0,-18} {1,-22} {2}' -f $dep, $status, $found))
    }

    function Add-Skipped([string] $dep, [string] $command) {
        [void] $state.Skipped.Add("  ${dep}: $command")
    }

    function Get-Version([string] $exe, [string[]] $arguments) {
        if (-not (Get-Command $exe -ErrorAction SilentlyContinue)) { return '' }
        # Windows PowerShell turns redirected native stderr into errors.
        $ErrorActionPreference = 'Continue'
        try {
            $out = (& $exe @arguments 2>$null | Out-String)
        } catch {
            return ''
        }
        if ($out -match '(\d+(\.\d+)+)') { return $Matches[1] }
        return ''
    }

    function Test-VersionAtLeast([string] $have, [string] $want) {
        try { return [version] (($have -split '\.')[0..2] -join '.') -ge [version] $want } catch { return $false }
    }

    # Consent rules: nothing without a yes; admin rights never silently.
    function Invoke-DepInstall([string] $dep, [bool] $needsAdmin, [string] $exe, [string[]] $arguments) {
        $shown = (@($exe) + $arguments) -join ' '
        $state.Result = 'skipped'
        if ($dryRun) {
            Write-Sub plan $shown
            if ($needsAdmin) {
                Write-Note 'needs administrator rights (a UAC prompt)'
                if (-not $interactive -and -not $allowAdmin) { Write-Note 'non-interactive: runs only with GUARD_ALLOW_SUDO=1' }
            }
            $state.Result = 'planned'
            return
        }
        if ($needsAdmin) { Write-Note "This needs administrator rights: $shown" }
        if ($interactive) {
            if (-not (Read-YesNo "Install $dep with `"$shown`"?" $false)) { Add-Skipped $dep $shown; return }
            if ($needsAdmin -and -not (Read-YesNo 'Run it with administrator rights?' $false)) { Add-Skipped $dep $shown; return }
        } else {
            if (-not $yes) {
                Write-Note 'skipped (non-interactive; set GUARD_YES=1 to install it)'
                Add-Skipped $dep $shown
                return
            }
            if ($needsAdmin -and -not $allowAdmin) {
                Write-Note 'skipped: needs administrator rights; set GUARD_ALLOW_SUDO=1 or run it yourself'
                Add-Skipped $dep $shown
                return
            }
        }
        if ($exe -eq 'winget') { Write-Note 'winget may show a Windows UAC prompt' }
        $ErrorActionPreference = 'Continue'
        try {
            & $exe @arguments 2>&1 | ForEach-Object { Write-Host "        $_" }
            $ok = ($LASTEXITCODE -eq 0)
        } catch {
            $ok = $false
        }
        if ($ok) {
            $state.Result = 'installed'
            # winget installs land on the machine PATH; pick them up now.
            $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User') + ';' + $env:Path
        } else {
            $state.Result = 'failed'
            Write-Sub warn "installing $dep failed; run it yourself: $shown"
            Add-Skipped $dep $shown
        }
    }

    # The installer's own question is the consent, so winget must not stop on
    # its source or package agreements (it would hang, or fail with GUARD_YES=1).
    $wingetFlags = @('--source', 'winget', '--accept-source-agreements', '--accept-package-agreements')

    function Invoke-Offer([string] $dep, [bool] $needsAdmin, [string] $id, [string] $url) {
        if (Get-Command winget -ErrorAction SilentlyContinue) {
            Invoke-DepInstall $dep $needsAdmin 'winget' (@('install', '-e', '--id', $id) + $wingetFlags)
        } elseif ($dryRun) {
            Write-Sub plan "winget install -e --id $id $($wingetFlags -join ' ') (or $url)"
            $state.Result = 'planned'
        } else {
            Write-Note "install it yourself: $url"
            Add-Skipped $dep "see $url"
            $state.Result = 'skipped'
        }
    }

    function Add-ResultRow([string] $dep, [string] $kind) {
        if ($state.Result -eq 'planned') { Add-Row $dep 'missing (planned)' '' } else { Add-Row $dep "missing ($kind)" '' }
    }

    function Test-Git {
        $min = if ($profileName -eq 'factory') { '2.31' } else { '' }
        $v = Get-Version git @('--version')
        if ($v -and (-not $min -or (Test-VersionAtLeast $v $min))) { Write-Sub ok "git $v"; Add-Row git ok $v; return }
        if ($v) { Write-Sub warn "git ${v}: needs >= $min for the PR publisher" }
        else { Write-Sub warn 'git: missing (required: doctor, affected, suspects, gate finalize)' }
        Invoke-Offer git $false 'Git.Git' 'https://git-scm.com/downloads'
        $v = Get-Version git @('--version')
        if ($state.Result -eq 'installed' -and $v) { Add-Row git ok $v } else { Add-ResultRow git required }
    }

    function Test-Node {
        $v = Get-Version node @('--version')
        $npm = Get-Version $npmExe @('--version')
        $npx = Get-Version $npxExe @('--version')
        if ($v -and (Test-VersionAtLeast $v '24.0')) {
            Write-Sub ok "node $v, npm $(if ($npm) { $npm } else { 'missing' }), npx $(if ($npx) { $npx } else { 'missing' })"
            Add-Row node ok $v
        } else {
            if ($v) { Write-Sub warn "node ${v}: needs >= 24" } else { Write-Sub warn 'node: missing (required: needs >= 24 with npm and npx)' }
            Invoke-Offer 'Node.js' $false 'OpenJS.NodeJS.LTS' 'https://nodejs.org/en/download'
            if ($state.Result -eq 'installed') {
                $v = Get-Version node @('--version')
                $npm = Get-Version $npmExe @('--version')
                $npx = Get-Version $npxExe @('--version')
            }
            if ($v -and (Test-VersionAtLeast $v '24.0')) { Add-Row node ok $v }
            elseif ($v) { Add-Row node 'needs >= 24' $v }
            else { Add-ResultRow node required }
        }
        Add-Row npm $(if ($npm) { 'ok' } else { 'missing (required)' }) $npm
        Add-Row npx $(if ($npx) { 'ok' } else { 'missing (required)' }) $npx
    }

    # Playwright uses PLAYWRIGHT_BROWSERS_PATH alone when it is set.
    function Find-Chromium {
        $dirs = if ($env:PLAYWRIGHT_BROWSERS_PATH) { @($env:PLAYWRIGHT_BROWSERS_PATH) } else { @(Join-Path $localAppData 'ms-playwright') }
        foreach ($dir in $dirs) {
            if (-not $dir -or $dir -eq '0' -or -not (Test-Path $dir)) { continue }
            $found = Get-ChildItem -Directory -Path $dir -Filter 'chromium-*' -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($found) { return $found.Name }
        }
        return ''
    }

    function Test-Chromium {
        $found = Find-Chromium
        if ($found) { Write-Sub ok "Playwright Chromium $found"; Add-Row chromium ok $found; return }
        Write-Sub warn 'Playwright Chromium: missing (recommended for guard test)'
        if (-not $dryRun -and -not (Get-Command $npxExe -ErrorAction SilentlyContinue)) {
            Write-Note 'needs Node.js and npx first; then run: npx -y playwright@1.64.1 install chromium'
            Add-Skipped chromium 'npx -y playwright@1.64.1 install chromium'
            Add-Row chromium 'missing (recommended)' ''
            return
        }
        Invoke-DepInstall 'Playwright Chromium' $false $npxExe @('-y', 'playwright@1.64.1', 'install', 'chromium')
        $found = Find-Chromium
        if ($state.Result -eq 'installed' -and $found) { Add-Row chromium ok $found } else { Add-ResultRow chromium recommended }
    }

    function Test-Pnpm {
        $v = Get-Version pnpm @('--version')
        if ($v) { Write-Sub ok "pnpm $v"; Add-Row pnpm ok $v; return }
        Write-Sub skip 'pnpm: missing (optional: gate finalize with a pnpm-lock.yaml)'
        Invoke-Offer pnpm $false 'pnpm.pnpm' 'https://pnpm.io/installation'
        $v = Get-Version pnpm @('--version')
        if ($state.Result -eq 'installed' -and $v) { Add-Row pnpm ok $v } else { Add-ResultRow pnpm optional }
    }

    function Test-Docker {
        $v = Get-Version docker @('--version')
        $compose = if ($v) { Get-Version docker @('compose', 'version') } else { '' }
        if ($v -and $compose) {
            Write-Sub ok "docker $v, compose $compose"
            Add-Row docker ok $v
            Add-Row 'docker compose' ok $compose
            $ErrorActionPreference = 'Continue'
            & docker info *> $null
            if ($LASTEXITCODE -ne 0) { Write-Sub warn 'the Docker daemon is not running; start Docker Desktop before guard test --docker' }
            return
        }
        if ($v) { Write-Sub warn "docker $v without Compose v2 (docker compose)" }
        else { Write-Sub warn 'docker: missing (required: test --docker, repro, agent lane, deploy stack)' }
        Write-Note 'Docker Desktop needs a paid subscription in larger companies (docker.com/pricing)'
        Invoke-Offer 'Docker Desktop' $true 'Docker.DockerDesktop' 'https://docs.docker.com/desktop/setup/install/windows-install/'
        if ($state.Result -eq 'installed') { Write-Note 'restart Windows or sign out and in, then start Docker Desktop' }
        Add-ResultRow docker required
    }

    function Test-Gh {
        $v = Get-Version gh @('--version')
        if ($v) { Write-Sub ok "gh $v"; Add-Row gh ok $v; return }
        Write-Sub skip 'gh: missing (optional: `gh auth token` gives a GITHUB_TOKEN for private repositories)'
        Invoke-Offer gh $false 'GitHub.cli' 'https://cli.github.com'
        $v = Get-Version gh @('--version')
        if ($state.Result -eq 'installed' -and $v) { Add-Row gh ok $v } else { Add-ResultRow gh optional }
    }

    # Deps for the chosen profile (step 5).
    function Test-Dependencies {
        if ($profileName -eq 'cli') {
            Write-Step 5 'Check dependencies' skip 'profile cli'
            Write-Note 'set GUARD_PROFILE=qa to check git, Node.js >= 24 and Playwright Chromium'
            return
        }
        $pm = if (Get-Command winget -ErrorAction SilentlyContinue) { 'winget' } else { 'none' }
        Write-Step 5 'Check dependencies' ok "profile $profileName, package manager $pm"
        Test-Git
        Test-Node
        Test-Chromium
        Test-Pnpm
        $curl = Get-Version curl.exe @('--version')
        if ($curl) { Write-Sub ok "curl $curl (https:// ready URLs)"; Add-Row curl ok $curl }
        else { Write-Sub skip 'curl: missing (optional: only for https:// ready URLs)'; Add-Row curl 'missing (optional)' '' }
        if ($profileName -ne 'factory') { return }
        Test-Docker
        Test-Gh
        $agents = @('claude', 'codex' | Where-Object { Get-Command $_ -ErrorAction SilentlyContinue }) -join ', '
        if (-not $agents) { $agents = 'none found' }
        Write-Sub skip "agent CLIs: $agents (not required on the host; they run in the sandbox image)"
        Add-Row 'agent CLIs' 'not required on host' $agents
        Write-Note 'Postgres and LiteLLM are services: run the deploy stack (deploy/docker-compose.yml, see deploy/README.md) and set DATABASE_URL'
    }

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

    # Downloads the archive and its checksum into $work.
    function Get-Release([string] $archive, [string] $checksum) {
        if ($token) {
            # Private repositories: github.com download links need a browser
            # session, so resolve the assets through the REST API.
            $api = if ($version -eq 'latest') {
                "https://api.github.com/repos/$repo/releases/latest"
            } else {
                "https://api.github.com/repos/$repo/releases/tags/v$version"
            }
            if ($dryRun) { return "GET $api (with your token), then $asset and $asset.sha256" }
            try {
                $release = Invoke-RestMethod -UserAgent 'guard-installer' -Uri $api -Headers (Get-Headers 'application/vnd.github+json')
            } catch {
                Fail "cannot find $label of $repo (check the token's access, or that a release exists)"
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
            if ($dryRun) { return "download $base/$asset and $asset.sha256" }
            foreach ($pair in @(@($asset, $archive), @("$asset.sha256", $checksum))) {
                try {
                    Invoke-WebRequest -UseBasicParsing -UserAgent 'guard-installer' -Uri "$base/$($pair[0])" -Headers (Get-Headers '*/*') -OutFile $pair[1]
                } catch {
                    Fail "cannot download $base/$($pair[0]) (no release yet? a private repository needs GITHUB_TOKEN or GH_TOKEN)"
                }
            }
        }
        return ''
    }

    # GUARD_ASSET_FILE: copy the local archive and write its checksum file, so
    # it goes through the same verify-then-extract code as a download.
    function Use-LocalAsset([string] $archive, [string] $checksum) {
        $file = $env:GUARD_ASSET_FILE
        if (-not (Test-Path -PathType Leaf $file)) { Fail "GUARD_ASSET_FILE does not exist: $file" }
        $hex = $env:GUARD_ASSET_SHA256
        if (-not $hex) {
            if (-not (Test-Path -PathType Leaf "$file.sha256")) {
                Fail "no checksum for ${file}: set GUARD_ASSET_SHA256 or put $file.sha256 next to it"
            }
            $hex = ((Get-Content -Raw "$file.sha256").Trim() -split '\s+')[0]
        }
        $hex = $hex.ToLowerInvariant()
        if ($hex -notmatch '^[0-9a-f]{64}$') { Fail 'the expected checksum is not a hex sha256 digest' }
        if ($dryRun) { return "use $file, verify sha256 $hex" }
        Copy-Item -Force $file $archive
        [IO.File]::WriteAllText($checksum, ('{0}  {1}' -f $hex, $asset) + "`n")
        return ''
    }

    # Verify, then extract: never unpack an archive whose checksum differs.
    # The one extraction site for downloads and GUARD_ASSET_FILE alike.
    function Confirm-AndExpand([string] $archive, [string] $checksum) {
        $expected = ((Get-Content -Raw $checksum).Trim() -split '\s+')[0].ToLowerInvariant()
        $actual = (Get-FileHash -Algorithm SHA256 -Path $archive).Hash.ToLowerInvariant()
        if ($expected -ne $actual) { Fail "checksum mismatch for $asset" }

        $out = Join-Path $work 'out'
        New-Item -ItemType Directory -Force -Path $out | Out-Null
        # Relative paths: a GNU tar earlier on PATH would take "C:" for a host.
        Push-Location $work
        try {
            tar -xzf $asset -C out | Out-Null
            $code = $LASTEXITCODE
        } finally {
            Pop-Location
        }
        if ($code -ne 0) { Fail "cannot extract $asset" }
        $exe = Join-Path $out 'guard.exe'
        if (-not (Test-Path $exe)) { Fail "$asset does not contain guard.exe" }
        return $exe
    }

    function Test-MsvcTools {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (-not (Test-Path $vswhere)) { return $false }
        $ErrorActionPreference = 'Continue'
        $path = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
        return [bool] $path
    }

    function Install-FromSource {
        $mark = if ($dryRun) { 'plan' } else { 'ok' }
        Write-Step 3 'Build from source' $mark "cargo install from https://github.com/$repo"
        $rustc = Get-Version rustc @('--version')
        $hasCargo = [bool] (Get-Command cargo -ErrorAction SilentlyContinue)
        if ($hasCargo -and $rustc -and (Test-VersionAtLeast $rustc '1.88')) {
            Write-Sub ok "rustc $rustc, cargo"
        } elseif ($hasCargo -and $rustc -and (Get-Command rustup -ErrorAction SilentlyContinue)) {
            Write-Sub warn "rustc $rustc is older than 1.88 (Cargo.toml rust-version)"
            Invoke-DepInstall Rust $false 'rustup' @('update', 'stable')
        } else {
            Write-Sub warn 'Rust (cargo, rustc >= 1.88) is missing'
            if (Get-Command winget -ErrorAction SilentlyContinue) {
                Invoke-DepInstall 'Rust (official rustup)' $false 'winget' (@('install', '-e', '--id', 'Rustlang.Rustup') + $wingetFlags)
            } else {
                $init = Join-Path $work 'rustup-init.exe'
                if ($dryRun) {
                    Write-Sub plan "download https://win.rustup.rs/x86_64 to $init, then $init -y --profile minimal"
                } elseif ($interactive -and (Read-YesNo 'Install Rust with the official rustup-init.exe?' $false) -or (-not $interactive -and $yes)) {
                    Invoke-WebRequest -UseBasicParsing -UserAgent 'guard-installer' -Uri 'https://win.rustup.rs/x86_64' -OutFile $init
                    $ErrorActionPreference = 'Continue'
                    & $init -y --profile minimal 2>&1 | ForEach-Object { Write-Host "        $_" }
                    $state.Result = if ($LASTEXITCODE -eq 0) { 'installed' } else { 'failed' }
                } else {
                    Add-Skipped Rust 'https://rustup.rs'
                }
            }
            $env:Path = (Join-Path $env:USERPROFILE '.cargo\bin') + ';' + $env:Path
        }
        if (-not (Test-MsvcTools)) {
            Write-Sub warn 'the MSVC Build Tools (C++ workload) are required to build Rust code on Windows'
            Invoke-DepInstall 'MSVC Build Tools' $true 'winget' (@('install', '-e', '--id', 'Microsoft.VisualStudio.2022.BuildTools') + $wingetFlags + @('--override', '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'))
        }
        $cargoArgs = @('install', '--locked', '--root', (Join-Path $work 'cargo'), '--git', "https://github.com/$repo")
        if ($version -ne 'latest') { $cargoArgs += @('--tag', "v$version") } else { Write-Note 'no version given: building the default branch' }
        $cargoArgs += @('guard-cli', '--bin', 'guard')
        Write-Note 'a private repository uses your git credentials (e.g. gh auth setup-git); no token is passed to cargo'
        if ($dryRun) { Write-Sub plan ("cargo " + ($cargoArgs -join ' ')); return '' }
        $rustc = Get-Version rustc @('--version')
        if (-not ($rustc -and (Test-VersionAtLeast $rustc '1.88'))) {
            Fail 'building from source needs Rust >= 1.88: set GUARD_YES=1 to install it with rustup, or install it from https://rustup.rs'
        }
        $ErrorActionPreference = 'Continue'
        & cargo @cargoArgs 2>&1 | ForEach-Object { Write-Host "        $_" }
        $built = Join-Path $work 'cargo\bin\guard.exe'
        if (-not (Test-Path $built)) { Fail 'cargo did not build guard.exe (see the output above)' }
        Write-Sub ok 'built guard.exe'
        return $built
    }

    function Write-Summary([string] $installed, [string] $guardVersion) {
        Write-Host ''
        Write-Line 'Summary' 'White'
        if ($dryRun) {
            Write-Host '  dry run: nothing was downloaded or installed'
            Write-Host "  would install: $installed"
        } else {
            Write-Host "  installed: $installed"
            Write-Host "  version:   $guardVersion"
            if ($fromSource) { Write-Host '  built from source (unverified checksum: n/a)' } else { Write-Host '  checksum:  sha256 verified' }
        }
        if ($state.Rows.Count -gt 0) {
            Write-Host ''
            Write-Host "  Dependencies (profile $profileName):"
            $state.Rows | ForEach-Object { Write-Host $_ }
        }
        if ($state.Skipped.Count -gt 0) {
            Write-Host ''
            Write-Host '  Skipped; run these yourself when you need them:'
            $state.Skipped | ForEach-Object { Write-Host $_ }
        }
        $onPath = ($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $installDir.TrimEnd('\') }
        if (-not $onPath) {
            Write-Host ''
            if (-not $dryRun -and (Read-YesNo "Add $installDir to your user PATH?" $false)) {
                [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ";$installDir", 'User')
                $env:Path = "$env:Path;$installDir"
                Write-Host "  added $installDir to your user PATH (new terminals pick it up)"
            } else {
                Write-Host "$installDir is not on your PATH. Add it for your user with:"
                Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$installDir', 'User')"
                Write-Host 'then open a new terminal.'
            }
        }
        Write-Host ''
        Write-Host 'Next steps:'
        Write-Host '  cd <your app repo>'
        Write-Host '  guard init            # detects your app, writes target.toml and e2e-playwright/'
        Write-Host '  guard init --install  # same, then npm install + playwright chromium'
        Write-Host '  guard doctor          # checks config, node, npx, git, docker, readiness'
        Write-Host '  guard up; guard test'
    }

    # 1. Platform
    $arch = if ($env:GUARD_ARCH) { $env:GUARD_ARCH } elseif ($env:PROCESSOR_ARCHITECTURE) { $env:PROCESSOR_ARCHITECTURE } else { 'AMD64' }
    $target = ''
    switch ($arch) {
        'AMD64' { $target = 'x86_64-pc-windows-msvc' }
        'x86_64' { $target = 'x86_64-pc-windows-msvc' }
        'ARM64' { $target = 'x86_64-pc-windows-msvc' }
    }
    $label = if ($version -eq 'latest') { 'the latest release' } else { "v$version" }
    $shownTarget = if ($fromSource -or -not $target) { "$target, from source".TrimStart(', ') } else { $target }
    $dot = [char] 0x00B7
    Write-Line "guard installer  $dot  $label  $dot  $shownTarget" 'White'
    if ($dryRun) { Write-Host 'dry run: printing the plan only' }
    Write-Host ''
    if ($target) {
        Write-Step 1 'Detect platform' ok $target
        # Windows on Arm runs the x64 binary through emulation.
        if ($arch -eq 'ARM64') { Write-Note 'note: no ARM64 build is published; installing the x64 binary' }
    } else {
        Write-Step 1 'Detect platform' warn "Windows $($arch): no prebuilt binary"
        if (-not $fromSource) {
            if (Read-YesNo 'Build guard from source with cargo (rustup is offered if Rust is missing)?' $false) { $fromSource = $true }
            else { Fail "no prebuilt binary for Windows ${arch}; set GUARD_FROM_SOURCE=1 to build it with cargo (rustup is offered when Rust is missing)" }
        }
    }

    # 2. Profile
    if (-not $profileName) {
        if ($interactive) {
            Write-Host '      Which tools should the installer check for you?'
            Write-Host '        1) cli      the guard binary only'
            Write-Host '        2) qa       + git, Node.js >= 24, Playwright Chromium (recommended)'
            Write-Host '        3) factory  + Docker with Compose v2, gh, agent CLIs'
            $answer = Read-Host '      Profile [2]'
            $profileName = switch -Regex ($answer) { '^(1|cli)$' { 'cli' } '^(3|factory)$' { 'factory' } default { 'qa' } }
        } else {
            $profileName = 'cli'
        }
    }
    Write-Step 2 'Choose profile' ok $profileName

    $asset = "guard-$target.tar.gz"
    $work = Join-Path ([IO.Path]::GetTempPath()) ("guard-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    $installed = Join-Path $installDir 'guard.exe'
    $guardVersion = ''
    try {
        # 3. Download and verify, or build
        $built = ''
        if (-not $fromSource) {
            $archive = Join-Path $work $asset
            $checksum = Join-Path $work "$asset.sha256"
            $s3 = if ($env:GUARD_ASSET_FILE) { 'Use local archive' } else { 'Download and verify' }
            if ($env:GUARD_ASSET_FILE) {
                $plan = Use-LocalAsset $archive $checksum
            } else {
                try {
                    $plan = Get-Release $archive $checksum
                } catch {
                    Write-Step 3 $s3 fail "$_"
                    if (Read-YesNo 'no prebuilt release found; build from source?' $false) { $fromSource = $true }
                    else { Fail "$($_ -replace '^error: ', ''); set GUARD_FROM_SOURCE=1 to build it with cargo" }
                }
            }
            if (-not $fromSource) {
                if ($dryRun) {
                    Write-Step 3 $s3 plan $asset
                    Write-Sub plan $plan
                    Write-Sub plan 'verify sha256 (Get-FileHash), then extract guard.exe'
                } else {
                    if (-not (Get-Command tar -ErrorAction SilentlyContinue)) {
                        Fail 'tar.exe is required (included in Windows 10 1803 and later)'
                    }
                    $built = Confirm-AndExpand $archive $checksum
                    Write-Step 3 $s3 ok "$asset, sha256 verified"
                }
            }
        }
        if ($fromSource) { $built = Install-FromSource }

        # 4. Install
        if ($dryRun) {
            Write-Step 4 'Install binary' plan $installed
        } else {
            New-Item -ItemType Directory -Force -Path $installDir | Out-Null
            # A running guard.exe (guard mcp, guard-gui) cannot be overwritten,
            # but it can be renamed: copy next to it, move it aside, move in.
            $staged = "$installed.new"
            $aside = "$installed.old"
            Copy-Item -Force $built $staged
            try {
                if (Test-Path $installed) {
                    Remove-Item -Force $aside -ErrorAction SilentlyContinue
                    if (Test-Path $aside) { $aside = "$installed.$([Guid]::NewGuid().ToString('N')).old" }
                    Move-Item -Force $installed $aside
                }
                Move-Item -Force $staged $installed
            } catch {
                Remove-Item -Force $staged -ErrorAction SilentlyContinue
                if (-not (Test-Path $installed) -and (Test-Path $aside)) { Move-Item -Force $aside $installed -ErrorAction SilentlyContinue }
                Fail "cannot replace ${installed}: stop running guard (guard mcp, guard-gui) and rerun"
            }
            $guardVersion = (& $installed --version | Out-String).Trim()
            if ($LASTEXITCODE -ne 0) { Fail "$installed --version failed" }
            Write-Step 4 'Install binary' ok $installed
            Write-Host "installed $installed"
        }
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }

    # 5. Dependencies, 6. Summary
    Test-Dependencies
    Write-Summary $installed $guardVersion
}

# A script block, so none of these variables (the token included) stays in
# the session that ran iex.
& {
    # Only the downloads need the token: keep it out of the environment of
    # winget, rustup, cargo build scripts and npx while the install runs, and
    # put the session's variables back afterwards.
    $saved = @{ GITHUB_TOKEN = $env:GITHUB_TOKEN; GH_TOKEN = $env:GH_TOKEN }
    $token = if ($saved.GITHUB_TOKEN) { $saved.GITHUB_TOKEN } elseif ($saved.GH_TOKEN) { $saved.GH_TOKEN } else { '' }
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
    try {
        Install-Guard $token
    } catch {
        Write-Host $_ -ForegroundColor Red
        if ($PSCommandPath) { exit 1 }
        # Under iex, `exit` would close an interactive window. Report the
        # failure through $LASTEXITCODE there; a batch host (powershell
        # -Command / -File, as CI and scripts run it) also exits 1 once its
        # command is done.
        $global:LASTEXITCODE = 1
        $hostArgs = @([Environment]::GetCommandLineArgs() | Select-Object -Skip 1)
        $batch = @($hostArgs | Where-Object { $_ -match '^[-/](c|co|com|comm|comma|comman|command|e|ec|en|enc|enco|encod|encode|encoded\w*|f|fi|fil|file|noni\w*)$' }).Count -gt 0
        $noExit = @($hostArgs | Where-Object { $_ -match '^[-/]noe\w*$' }).Count -gt 0
        if ($batch -and -not $noExit) { $Host.SetShouldExit(1) }
    } finally {
        foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    }
}
