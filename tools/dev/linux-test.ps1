<#
.SYNOPSIS
  Runs cargo on Linux (Docker) for this checkout, with all build state in named volumes.

.DESCRIPTION
  Builds or reuses the local image gitcthulhu-linux-dev:<toolchain>-<hash>: rust:1-trixie with the
  Linux packages the workspace needs (.github/workflows/ci.yaml and scripts/install-minimal-debian-
  dependencies.sh: libdbus for the keyring, libxcb / xkbcommon-x11 / fontconfig / wayland for the
  gpui-kit GUI), Zig 0.17 (but-tty builds libghostty-vt with it; keep the version in sync with
  .github/actions/setup-zig) and the toolchain that rustup installs from this checkout's
  rust-toolchain.toml, running as the non-root user dev (uid 1000), so permission tests run as on
  CI instead of skipping as root. The tag differs per rust-toolchain.toml and image definition, so
  checkouts on different toolchains never run each other's image. Then runs `cargo <arguments>` in a
  throw-away container (--rm) named gitcthulhu-tmp-cargo-<checkout>-<random>:

    /work                               this checkout (bind mount)
    /target (CARGO_TARGET_DIR)          volume gitcthulhu-target-main-<hash> for the main
                                        checkout (one per clone of the repository),
                                        gitcthulhu-target-wt-<folder>-<hash> for a worktree
    CARGO_HOME (registry, git, and      shared volume gitcthulhu-cargo-home: one volume, so
    the locks that guard them)          concurrent containers share cargo's cache locks

  --git-min runs the same in a second image, gitcthulhu-linux-dev:gitmin-<toolchain>-<hash>, built
  from ubuntu:22.04 so that `git` is 2.34.1, the oldest Git the backend supports (spec 20 section
  1.3), with its own volumes (gitcthulhu-target-gitmin-main-<hash>, gitcthulhu-target-gitmin-wt-...:
  another glibc and compiler, so another target directory); the cargo cache volume is shared.

  Everything this script creates is named gitcthulhu-*; the image and the containers are
  labelled gitcthulhu.dev, target volumes gitcthulhu.worktree=<checkout path> and
  gitcthulhu.clone=<main checkout path>. Containers also carry the process id and start
  time of the pwsh that started them: a container whose pwsh is gone (killed by a tool timeout,
  for one) is removed by the next run, --drop-volume or --gc. The Docker disk grows by these
  volumes, the images and BuildKit's build cache (which Docker's builder garbage collection keeps
  bounded).

  A worktree's new target volume is seeded from its clone's main volume (of the same flavor):
  debug/ only, without the repository's own build output (every path package and its targets), with
  sub-second timestamps kept, and under cargo's build locks in the main volume, so a build there
  cannot change it halfway. Its first build compiles only the workspace crates; the paths are the
  same in every container, so the seed is reused as is. Concurrent runs for the same checkout wait
  until its volume is ready, and a copy that was killed halfway is redone (lock files and seed
  markers in .git\gitcthulhu-dev). Build in the main checkout once to create the seed; a seed from
  an older main checkout is slower, not wrong. CARGO_BUILD_JOBS, GITCTHULHU_CAPTURE_FIXTURES,
  GITCTHULHU_PERF_ASSERT and LC_ALL are passed through when set. To capture the Git 2.34.1 stderr
  fixtures: `$env:GITCTHULHU_CAPTURE_FIXTURES = '1'; pwsh -NoProfile -File tools/dev/linux-test.ps1
  --git-min test -p gitcthulhu-git --test cli -- --ignored capture_fixtures --exact`. CI's git-min
  job (the images have the de_DE.UTF-8 locale): `$env:LC_ALL = 'de_DE.UTF-8'; pwsh -NoProfile -File
  tools/dev/linux-test.ps1 --git-min test -p gitcthulhu-git`.

  Permanent: the main volume of every clone and flavor (the seed of its worktree volumes) and the
  shared cache gitcthulhu-cargo-home, which this script never deletes. Losing them costs a cold
  build and the downloads again. The images are rebuilt automatically when missing.

  Options (before the cargo arguments):
    --git-min       use the Git 2.34.1 image and its volumes (see above); combines with every
                    other option, which then act on that flavor
    --update        pull the newest rust:1-trixie, rebuild this checkout's image and remove the
                    image it replaced (once at the start of a session); alone, it runs no cargo
                    command. With --git-min the local ubuntu:22.04 is not re-pulled (it may be
                    someone else's tag): the image is rebuilt from the build cache
    --prune-images  remove every other tag of this flavor's image (other toolchains, other image
                    definitions such as another branch's) that no container uses. Only while no
                    other checkout is starting a Linux build: a run that has checked its tag but
                    not yet started its container would fail with "No such image". A checkout that
                    needs a removed tag rebuilds it from the build cache.
    --drop-volume   delete this worktree's target volume (when the worktree is done); refused in
                    the main checkout
    --gc            delete the target volumes (both flavors) of this clone's worktrees that no
                    longer exist at their path, and unlabelled worktree volumes (docker creates one
                    by itself when a run mounts a volume that was removed in the meantime); then
                    the markers and lock files of volumes and worktrees that are gone. Main
                    volumes and other clones' volumes are never touched. Stops before removing
                    anything when Docker does not answer.

  Call it as `pwsh -NoProfile -File` (as in the examples). From a PowerShell session
  (& ./tools/dev/linux-test.ps1), PowerShell splits an argument that starts with '-' at its first
  ':' (--skip=a::b, -Dclippy::all) and drops a bare --; there, write --skip a::b, -D clippy::all
  and '--'. From Git Bash, a container path among the arguments (/work/...) needs
  MSYS_NO_PATHCONV=1 in front of the command (see AGENTS.md).

.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 test -p gitcthulhu-git --locked
.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 --git-min test -p gitcthulhu-git --locked
.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 clippy --workspace --all-targets --locked -- -D warnings
.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 --update
#>
#Requires -Version 7.3
$ErrorActionPreference = 'Stop'
# Native commands: exit codes are checked explicitly, whatever the calling session set.
$PSNativeCommandUseErrorActionPreference = $false
$PSNativeCommandArgumentPassing = 'Windows'
Import-Module (Join-Path $PSScriptRoot 'common.psm1') -Force -DisableNameChecking

# The arguments as given. PowerShell splits one that starts with '-' at its first ':' (its
# -Name:Value syntax), so --skip=a::b would reach cargo as '--skip=a' ':b'. Under pwsh -File the
# process command line still holds them unchanged; called from a session, $args is all there is.
$given = @($args)
$commandLine = [Environment]::GetCommandLineArgs()
for ($i = 0; $i -lt $commandLine.Count; $i++) {
    if ($commandLine[$i] -like '*.ps1' -and (Test-Path -LiteralPath $commandLine[$i] -PathType Leaf) -and
        (Test-SamePath (Resolve-Path -LiteralPath $commandLine[$i]).Path $PSCommandPath)) {
        $given = @($commandLine | Select-Object -Skip ($i + 1))
        break
    }
}

$update = $false; $pruneImages = $false; $drop = $false; $gc = $false; $gitMin = $false
$rest = $given
while ($rest.Count -gt 0 -and $rest[0] -in '--update', '--prune-images', '--drop-volume', '--gc', '--git-min') {
    switch ($rest[0]) {
        '--git-min' { $gitMin = $true }
        '--update' { $update = $true }
        '--prune-images' { $pruneImages = $true }
        '--drop-volume' { $drop = $true }
        '--gc' { $gc = $true }
    }
    $rest = @($rest | Select-Object -Skip 1)
}
$cargoArgs = $rest
if (-not ($update -or $pruneImages -or $drop -or $gc -or $cargoArgs.Count)) {
    throw 'Nothing to do: pass cargo arguments or --update / --prune-images / --drop-volume / --gc (see Get-Help). --git-min only selects the Git 2.34.1 image.'
}

# Every Docker object of this script starts with this prefix, so it cannot be mistaken for the
# user's own images, containers and volumes.
$prefix = 'gitcthulhu'
$repository = "$prefix-linux-dev"
# The two flavors: the default image (rust:1-trixie, the distribution's current Git) and --git-min
# (ubuntu:22.04: Git 2.34.1). Another glibc and compiler, so every flavor has its own target
# volumes, and the git-min names carry "gitmin" right after "target-" / "linux-dev:".
$flavor = if ($gitMin) { 'gitmin-' } else { '' }
$baseImage = if ($gitMin) { 'ubuntu:22.04' } else { 'rust:1-trixie' }
$root = Get-CheckoutRoot
$owner = "${PID}:$((Get-Process -Id $PID).StartTime.ToFileTimeUtc())"

function Invoke-Docker {
    docker @args
    if ($LASTEXITCODE -ne 0) { throw "docker $($args[0]) failed with exit code $LASTEXITCODE." }
}

# All volume names. A failed listing stops the script: it must never read as "the volume is gone"
# (that would, for one, drop the marker of a half-filled volume).
function Get-VolumeNames {
    $names = Invoke-Utf8 { docker volume ls --format '{{.Name}}' }
    if ($LASTEXITCODE -ne 0) { throw 'Docker does not answer (docker volume ls failed).' }
    @($names)
}

# Removes the containers of this script whose pwsh is gone: they keep running in the daemon,
# building old sources and holding their target volume.
function Remove-OrphanContainers {
    $lines = Invoke-Utf8 { docker ps -a --filter "label=$prefix.owner" --format "{{.ID}}`t{{.Names}}`t{{.Label `"$prefix.owner`"}}" }
    if ($LASTEXITCODE -ne 0) { throw 'Docker does not answer (docker ps failed).' }
    foreach ($line in $lines) {
        $id, $name, $label = $line -split "`t", 3
        $ownerPid, $started = $label -split ':', 2
        $process = Get-Process -Id ([int]$ownerPid) -ErrorAction SilentlyContinue
        # A process whose start time cannot be read (another user's) counts as alive.
        $alive = $process -and $(try { $process.StartTime.ToFileTimeUtc() -eq [long]$started } catch { $true })
        if (-not $alive) {
            docker rm -f $id *> $null
            if ($LASTEXITCODE -eq 0) { Write-Host "Removed $name (the run that started it is gone)." }
        }
    }
}

# This checkout's name in the volume and container names: main-<hash of its path> for the main
# checkout (first `git worktree list` entry; the hash keeps two clones of the repository apart),
# wt-<folder>-<hash of its path> for a worktree, so that no worktree can look like a main volume.
$main = Get-MainCheckout $root
$isMain = Test-SamePath $root $main
$mainVolume = "$prefix-target-${flavor}main-$(Get-PathHash $main)"
if ($isMain) {
    $checkout = "main-$(Get-PathHash $main)"
} else {
    $leaf = ((Split-Path -Leaf $root).ToLowerInvariant() -replace '[^a-z0-9_.-]', '-')
    $checkout = "wt-$leaf-$(Get-PathHash $root)"
}
$volume = "$prefix-target-$flavor$checkout"

# The lock file and the seed marker of a volume, in this clone's .git.
$stateDir = Get-StateDir $root
function Get-LockPath([string]$Name) { Join-Path $stateDir "$Name.lock" }
function Get-SeedMarker([string]$Name) { Join-Path $stateDir "$Name.seeding" }

if ($gc) {
    Remove-OrphanContainers
    $listing = Invoke-Utf8 { docker volume ls --format "{{.Name}}`t{{.Label `"$prefix.worktree`"}}`t{{.Label `"$prefix.clone`"}}" }
    if ($LASTEXITCODE -ne 0) { throw 'Docker does not answer (docker volume ls failed); no volume was removed.' }
    $wtVolume = "^$([regex]::Escape($prefix))-target-(gitmin-)?wt-[a-z0-9_.-]+-[0-9a-f]{8}$"
    foreach ($line in $listing) {
        $name, $path, $clone = $line -split "`t", 3
        if ($name -notmatch $wtVolume) { continue }
        if ($path) {
            # Only this clone's: another clone may sit on a drive that is not mounted right now.
            if (-not $clone -or -not (Test-SamePath $clone $main) -or (Test-Path -LiteralPath $path)) { continue }
        }
        docker volume rm $name *> $null
        if ($LASTEXITCODE -ne 0) { Write-Warning "$name is in use; kept." }
        elseif ($path) { Write-Host "Removed $name ($path no longer exists)." }
        else { Write-Host "Removed $name (no checkout label)." }
    }
    # Markers of volumes that are gone, each under its volume's lock: a seed writes its marker
    # before it creates the volume and holds the lock until the copy is done.
    foreach ($marker in @(Get-ChildItem -LiteralPath $stateDir -Filter '*.seeding' -ErrorAction SilentlyContinue)) {
        Invoke-Locked (Get-LockPath $marker.BaseName) {
            if ((Get-VolumeNames) -notcontains $marker.BaseName) {
                Remove-Item -LiteralPath $marker.FullName -ErrorAction SilentlyContinue
            }
        }
    }
    # Lock files of volumes and worktrees that are gone (seed-target.ps1's are named by the worktree
    # path's hash). Deleted by opening them exclusively with DeleteOnClose: a run opening one
    # meanwhile sees it busy and waits, instead of hitting a file whose deletion is pending.
    $volumes = Get-VolumeNames
    $worktreeLocks = @(Get-Worktrees $root | ForEach-Object { "seed-target-$(Get-PathHash $_)" })
    foreach ($lockFile in @(Get-ChildItem -LiteralPath $stateDir -Filter '*.lock' -ErrorAction SilentlyContinue)) {
        $gone = ($lockFile.BaseName -like "$prefix-target-*" -and $volumes -notcontains $lockFile.BaseName) -or
            ($lockFile.BaseName -like 'seed-target-*' -and $worktreeLocks -notcontains $lockFile.BaseName)
        if (-not $gone) { continue }
        try {
            # The options as an enum: a string would bind to the overload's bool useAsync instead.
            [System.IO.FileStream]::new($lockFile.FullName, [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None, 1, [System.IO.FileOptions]::DeleteOnClose).Dispose()
        } catch { }   # held right now: it stays
    }
}
if ($drop) {
    if ($isMain) {
        Write-Warning "$volume is the seed of every worktree volume and is kept; --drop-volume is for worktrees."
    } else {
        Remove-OrphanContainers
        # Under the volume's lock, so that it never removes a volume that is being seeded.
        Invoke-Locked (Get-LockPath $volume) {
            if ((Get-VolumeNames) -contains $volume) {
                Invoke-Docker volume rm $volume | Out-Null
                Write-Host "Removed $volume."
            } else {
                Write-Host "$volume does not exist."
            }
            Remove-Item -LiteralPath (Get-SeedMarker $volume) -ErrorAction SilentlyContinue
        }
    }
}
if (-not ($update -or $pruneImages -or $cargoArgs.Count)) { exit 0 }

# The image: built when its tag is missing, rebuilt from the newest base image with --update.
$toolchainFile = (Get-Content -Raw -LiteralPath (Join-Path $root 'rust-toolchain.toml')) -replace "`r`n", "`n"
# For the tag only; rustup reads the file itself in the image.
$toolchain = if ($toolchainFile -match '(?m)^\s*channel\s*=\s*["'']([^"'']+)["'']') { $Matches[1] -replace '[^A-Za-z0-9_.-]', '-' } else { 'toolchain' }
$dockerfile = @'
# BASE_IMAGE: rust:1-trixie (default) or ubuntu:22.04 (--git-min: its git is 2.34.1). Not an older
# Debian for the default: newer glibc and GCC are what the dependencies' C and Zig code is
# routinely built with.
ARG BASE_IMAGE=rust:1-trixie
FROM ${BASE_IMAGE}
LABEL gitcthulhu.dev=1
ENV DEBIAN_FRONTEND=noninteractive
# The Linux packages of .github/workflows/ci.yaml and scripts/install-minimal-debian-dependencies.sh
# (libdbus: the keyring in but-secret; xcb, xkbcommon-x11, fontconfig: gpui-kit), plus wayland, X11 and
# freetype headers for gpui's Linux platform layer and what the -sys crates compile (gcc, make,
# cmake, openssl). Keep in sync with CI. The de_DE.UTF-8 locale: with LC_ALL=de_DE.UTF-8 Git prints
# German messages unless the executor's LANGUAGE=en wins, which is what CI's git-min job checks.
# Ubuntu moves the translations (git.mo) out of the packages into language-pack-de-base, which also
# generates the locale; Debian ships them with git.
RUN . /etc/os-release \
 && apt-get update \
 && apt-get install -y --no-install-recommends build-essential ca-certificates cmake curl git pkg-config xz-utils \
    libssl-dev libdbus-1-dev libxcb1-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libwayland-dev libfontconfig-dev libfreetype-dev locales $([ "$ID" = ubuntu ] && echo language-pack-de-base) \
 && localedef -i de_DE -c -f UTF-8 de_DE.UTF-8 \
 && rm -rf /var/lib/apt/lists/*
# Zig for but-tty (libghostty-vt-sys). Keep the version in sync with .github/actions/setup-zig; the
# checksums are the ones in https://ziglang.org/download/index.json.
ARG ZIG_VERSION=0.17.0
ARG ZIG_SHA256_X86_64=1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026
ARG ZIG_SHA256_AARCH64=9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8
RUN arch="$(uname -m)" \
 && case "$arch" in x86_64) sha="$ZIG_SHA256_X86_64";; aarch64) sha="$ZIG_SHA256_AARCH64";; *) echo "no Zig for $arch"; exit 1;; esac \
 && curl -fsSL -o /tmp/zig.tar.xz "https://ziglang.org/download/${ZIG_VERSION}/zig-${arch}-linux-${ZIG_VERSION}.tar.xz" \
 && echo "$sha  /tmp/zig.tar.xz" | sha256sum -c - \
 && mkdir -p /opt/zig && tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1 \
 && ln -s /opt/zig/zig /usr/local/bin/zig && rm /tmp/zig.tar.xz && zig version
# rustup: the rust image has it; the Ubuntu one gets it here, at the same place.
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN command -v rustup >/dev/null \
 || (curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none --no-modify-path)
# rustup installs the checkout's rust-toolchain.toml itself (channel, profile, components, targets)
# and makes it the default.
ARG TOOLCHAIN_FILE
RUN mkdir -p /tmp/toolchain && printf '%s\n' "$TOOLCHAIN_FILE" > /tmp/toolchain/rust-toolchain.toml \
 && cd /tmp/toolchain && rustup toolchain install \
 && rustup default "$(rustup show active-toolchain | cut -d' ' -f1)" \
 && cd / && rm -rf /tmp/toolchain
# Mount points owned by dev: a new named volume takes over the owner of its mount point. CARGO_HOME
# moves out of /usr/local/cargo (whose bin/ keeps the rustup proxies on PATH) into its own volume.
RUN useradd -m -u 1000 dev \
 && mkdir -p /target /home/dev/.cargo \
 && chown dev:dev /target \
 && chown -R dev:dev /home/dev/.cargo
ENV CARGO_HOME=/home/dev/.cargo
USER dev
'@ -replace "`r`n", "`n"
# (LF: a checkout with core.autocrlf=true has CRLF here-strings, which break the RUN line
# continuations and would give the same image another tag.)
# One tag per rust-toolchain.toml and image definition: checkouts on different toolchains (a branch
# that bumps rust-toolchain.toml) build different tags, so a run never starts another checkout's
# image, which would lack its toolchain (and dev cannot install one into the root-owned rustup
# home).
$image = "${repository}:$flavor$toolchain-$(Get-ShortHash "$baseImage`n$dockerfile`n$toolchainFile")"
# Without provenance attestations (they carry build timestamps) a cached rebuild yields the same
# image ID, so the tag does not move on every run.
$build = @('build', '--provenance=false', '-t', $image, '--build-arg', "BASE_IMAGE=$baseImage", '--build-arg', "TOOLCHAIN_FILE=$toolchainFile")
if ($update -or $cargoArgs.Count) {
    # The tag pins rust-toolchain.toml and the image definition, so an existing tag needs no build.
    docker image inspect $image *> $null
    $missing = $LASTEXITCODE -ne 0
    if ($update) {
        # --git-min: no --pull, the local ubuntu:22.04 may be someone else's tag.
        $dockerfile | docker @build $(if (-not $gitMin) { '--pull' }) - | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "docker build failed with exit code $LASTEXITCODE." }
        # The image this rebuild replaced, now without a tag. Safe while other runs go on: they
        # start their containers by tag, which already names the new image.
        docker image prune -f --filter "label=$prefix.dev=1" | Out-Host
    } elseif ($missing) {
        $dockerfile | docker @build -q - | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "docker build failed with exit code $LASTEXITCODE." }
    }
}
if ($pruneImages) {
    # Not part of --update: another checkout's run may have checked its tag and not yet started its
    # container (docker only protects images that containers use).
    foreach ($other in (docker image ls $repository --format '{{.Repository}}:{{.Tag}}')) {
        # Only this flavor's tags: the other flavor's image is not a leftover of this one.
        $otherIsGitMin = $other -like "${repository}:gitmin-*"
        if ($other -ne $image -and $otherIsGitMin -eq $gitMin) {
            docker rmi $other *> $null
            if ($LASTEXITCODE -eq 0) { Write-Host "Removed $other." } else { Write-Host "Kept $other (a container uses it)." }
        }
    }
}
if (-not $cargoArgs.Count) { exit 0 }

Remove-OrphanContainers
$runId = Get-Random -Maximum 1000000

# This checkout's target volume (Invoke-Seed: lock, marker, redo of a copy killed halfway). A new
# worktree volume starts as a copy of the main one: debug/ only, without incremental/, examples/
# and the repository's own build output.
Invoke-Seed -Name $volume -LockPath (Get-LockPath $volume) -Marker (Get-SeedMarker $volume) `
    -Exists { (Get-VolumeNames) -contains $volume } `
    -Remove {
        Remove-OrphanContainers   # the copy of the killed run may still be going
        if ((Get-VolumeNames) -contains $volume) {
            docker volume rm $volume *> $null
            if ($LASTEXITCODE -ne 0) { throw "$volume is in use by a running build; run the script again when it has finished." }
        }
    } `
    -Create {
        Invoke-Docker volume create --label "$prefix.worktree=$root" --label "$prefix.clone=$main" $volume | Out-Null
        if ($isMain -or (Get-VolumeNames) -notcontains $mainVolume) { return }
        Write-Host "Seeding $volume from $mainVolume..."
        $names = Get-WorkspaceOutputNames $root
        $excludes = @('./debug/incremental', './debug/examples') + @(foreach ($n in $names.Packages) {
                "./debug/$n"; "./debug/$n.*"; "./debug/lib$n.*"; "./debug/deps/$n-*"; "./debug/deps/lib$n-*"
                "./debug/.fingerprint/$n-*"; "./debug/build/$n-*"
            }) + @(foreach ($n in $names.Executables) { "./debug/$n"; "./debug/deps/$n-*" }) | ForEach-Object { "--exclude=$_" }
        # Under cargo's locks in the main volume (flock, as cargo takes them; all or none, so it never
        # waits holding one), so a build there cannot change the source halfway. A missing lock file
        # is created, as cargo itself would, so that a cargo starting meanwhile waits for the copy
        # (the main volume is mounted writable for that alone). pipefail: a failed read fails the
        # seed even when the extraction succeeds. posix format: sub-second mtimes.
        $copy = @'
set -o pipefail
[ -d /from/debug ] || exit 0
fds=()
for f in /from/debug/.cargo-build-lock /from/debug/.cargo-artifact-lock; do
    exec {fd}>>"$f" || exit 1
    fds+=("$fd")
done
waiting=
while :; do
    got=()
    for fd in "${fds[@]}"; do
        if flock -xn "$fd"; then got+=("$fd"); else for g in "${got[@]}"; do flock -u "$g"; done; got=(); break; fi
    done
    [ "${#got[@]}" -eq "${#fds[@]}" ] && break
    [ -n "$waiting" ] || { echo "Waiting for cargo to finish building in the main volume..."; waiting=1; }
    sleep 0.5
done
tar -C /from --format=posix "$@" -cf - ./debug | tar -C /target -xf -
'@ -replace "`r`n", "`n"   # LF: bash fails on 'do\r' from a core.autocrlf=true checkout
        # --init with TINI_KILL_PROCESS_GROUP: Ctrl+C reaches the tars, not only bash.
        docker run --rm --init -e TINI_KILL_PROCESS_GROUP=1 --pull never --name "$prefix-tmp-seed-$flavor$checkout-$runId" `
            --label "$prefix.dev=1" --label "$prefix.owner=$owner" `
            --mount "type=volume,src=$mainVolume,dst=/from" `
            --mount "type=volume,src=$volume,dst=/target" $image bash -c $copy bash @excludes
        if ($LASTEXITCODE -ne 0) { throw "Seeding $volume failed with exit code $LASTEXITCODE; run the script again to redo it." }
    } | Out-Null

# --pull never: a local image only (it is never on a registry); if --prune-images removed it after
# the check above, the run fails with "No such image" instead of trying a registry. The bind
# source is quoted: --mount is comma-separated, and a path may contain commas.
$run = @('run', '--rm', '--init', '--pull', 'never', '--name', "$prefix-tmp-cargo-$flavor$checkout-$runId",
    '--label', "$prefix.dev=1", '--label', "$prefix.owner=$owner",
    '-w', '/work',
    '--mount', "type=bind,`"src=$root`",dst=/work",
    '--mount', "type=volume,src=$volume,dst=/target",
    '--mount', "type=volume,src=$prefix-cargo-home,dst=/home/dev/.cargo",

    '-e', 'CARGO_TARGET_DIR=/target', '-e', 'RUST_BACKTRACE=1')
# Passed through when set: the build parallelism, the switches of the test suites
# (GITCTHULHU_CAPTURE_FIXTURES=1 for the capture_fixtures test, GITCTHULHU_PERF_ASSERT=1 for the perf budgets)
# and LC_ALL (de_DE.UTF-8 for the locale run).
foreach ($name in 'CARGO_BUILD_JOBS', 'GITCTHULHU_CAPTURE_FIXTURES', 'GITCTHULHU_PERF_ASSERT', 'LC_ALL') {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ($value) { $run += @('-e', "$name=$value") }
}
docker @run $image cargo @cargoArgs
exit $LASTEXITCODE
