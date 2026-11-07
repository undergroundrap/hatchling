# Sets Hatchling up in a WSL distro of its own, from the release files beside
# this script. Run by the installer in a console the owner can see, or by hand:
#
#   setup.ps1 [-Distro hatchling-wsl] [-Release <dir>] [-Version <x.y.z>] [-Backup]
#             [-Hosted | -ModelEndpoint <url> -ModelName <name>]
#
# With neither, the model runs on this machine, with Ollama: one already running
# that has the model is used, and otherwise it asks before installing it. -Hosted uses OpenRouter, and the broker asks for its key
# at its own prompt in this console. -ModelEndpoint is a model server already
# running, at an address WSL can reach as localhost.
#
# On a distro that has Hatchling already, it updates it: settings, model and
# data stay, and none of the model options apply. -Backup makes a backup first,
# and a backup that fails stops the update before anything changes. A setup
# older than what's installed stops too, as what's installed may have moved
# Hatchling's data on.
#
# The owner's account in the distro has no password and no sudo: only Windows'
# `wsl -u root` runs the commands that need root, so the app's host, which
# runs as that account, gets no way to root.
#
# The distro is sealed from Windows: no drives mounted, and no starting Windows
# programs, so an account in it that isn't root can't reach this PC. Windows can
# still reach in, and that's how the release files get there.
param(
    [string] $Distro = 'hatchling-wsl',
    [string] $Release = (Join-Path $PSScriptRoot 'release'),
    [string] $Version = '',
    [switch] $Backup,
    [switch] $Hosted,
    [string] $ModelEndpoint = '',
    [string] $ModelName = 'qwen3.5:9b'
)
# Not 'Stop': in Windows PowerShell, a native program's stderr is an error, and
# each step here checks wsl.exe's exit code instead.
$ErrorActionPreference = 'Continue'
. (Join-Path $PSScriptRoot 'common.ps1')

function Say([string] $Words) { Write-Host "== $Words" -ForegroundColor Cyan }

if ($Distro -notmatch '^[A-Za-z0-9._-]+$') { Stop-Setup "That isn't a distro's name: $Distro" }
if (-not (Test-Path -LiteralPath (Join-Path $Release 'SHA256SUMS'))) { Stop-Setup "No release files in $Release." }

Say 'Checking WSL'
& wsl.exe --status *> $null
if ($LASTEXITCODE -ne 0) {
    Stop-Setup "Hatchling runs in WSL, which isn't on this PC yet. In PowerShell, run as administrator: wsl --install`nThen restart Windows, and run this setup again."
}

if ((Get-Distros) -notcontains $Distro) {
    Say "Making a WSL distro of its own, $Distro. This downloads Ubuntu from Microsoft, and takes a few minutes"
    & wsl.exe --install Ubuntu --name $Distro --no-launch
    if ($LASTEXITCODE -ne 0) { Stop-Setup "WSL couldn't make the distro $Distro. The lines above say why." }
}
if ((Get-FromWsl $Distro @('cat', '/proc/1/comm')) -ne 'systemd') {
    Stop-Setup "$Distro isn't running systemd, which Hatchling's service needs. Update WSL with: wsl --update"
}

$updating = (Get-FromWsl $Distro @('sh', '-c', 'test -f /etc/hatchling/hatchling.env && echo yes')) -eq 'yes'
if (-not $updating -and (Get-FromWsl $Distro @('sh', '-c', 'test -d /sys/fs/cgroup/system.slice/hatchling.service && echo yes')) -eq 'yes') {
    Stop-Setup 'Another copy of Hatchling is running in a different WSL distro, and two can''t run at once. Stop that one first, then run this setup again. Nothing was installed.'
}
if ($updating -and $Version) {
    $had = Get-FromWsl $Distro @('sh', '-c', 'cat /etc/hatchling/windows-setup-version 2>/dev/null')
    if ($had -and ([version] $had -gt [version] $Version)) {
        Stop-Setup "Hatchling $had is installed, which is newer than this setup ($Version). Nothing was changed."
    }
}
if ($updating -and $Backup -and (Get-FromWsl $Distro @('systemctl', 'is-active', 'hatchling')) -eq 'active') {
    Say 'Making a backup first. A vault protected by a security key asks for your PIN and a touch'
    & wsl.exe -d $Distro -u root --exec hatchling-broker backup
    if ($LASTEXITCODE -ne 0) { Stop-Setup 'The backup didn''t finish, so nothing was updated. The lines above say why.' }
}

& wsl.exe -d $Distro -u root --exec sh -c "id -u $Owner > /dev/null 2>&1 || useradd -m -s /bin/bash $Owner"
if ($LASTEXITCODE -ne 0) { Stop-Setup "Couldn't make the account '$Owner' in $Distro." }
$ownerUid = Get-FromWsl $Distro @('id', '-u', $Owner)

# Applied by restarting the distro, so it's in place before the broker first
# starts. An update comes after its backup, which needs the broker running.
$seal = 'grep -q ^.automount /etc/wsl.conf 2>/dev/null || printf ''\n[automount]\nenabled=false\n\n[interop]\nenabled=false\nappendWindowsPath=false\n'' >> /etc/wsl.conf'
if ((Get-FromWsl $Distro @('sh', '-c', 'test -e /mnt/c/Windows && echo open')) -eq 'open') {
    Say 'Sealing the distro off from Windows'
    & wsl.exe -d $Distro -u root --exec sh -c $seal
    if ($LASTEXITCODE -ne 0) { Stop-Setup "Couldn't seal $Distro off from Windows, so nothing was installed." }
    & wsl.exe --terminate $Distro
    if ((Get-FromWsl $Distro @('sh', '-c', 'test -e /mnt/c/Windows && echo open')) -eq 'open') {
        Stop-Setup "$Distro can still reach Windows' files, so nothing was installed."
    }
}

Say 'Checking every file against the release''s SHA256SUMS'
$from = '/root/hatchling-release'
& wsl.exe -d $Distro -u root --exec sh -c "rm -rf $from; mkdir -m 700 $from"
try {
    Copy-Item -Path (Join-Path $Release '*') -Destination "\\wsl.localhost\$Distro$($from -replace '/', '\')" -ErrorAction Stop
}
catch {
    Stop-Setup "Couldn't copy the release files into ${Distro}: $_"
}
$unpack = 'set -e; sha256sum --check --ignore-missing --quiet SHA256SUMS; grep -q hatchling-installer.tar.gz SHA256SUMS; rm -rf /root/hatchling-installer; mkdir -m 700 /root/hatchling-installer; tar -xzf hatchling-installer.tar.gz -C /root/hatchling-installer --no-same-owner --no-same-permissions'
& wsl.exe -d $Distro -u root --cd $from --exec sh -c $unpack
if ($LASTEXITCODE -ne 0) { Stop-Setup 'A file differs from the release''s SHA256SUMS: nothing was installed.' }

$model = @()
if ($updating) {
    Say 'Updating Hatchling. Your settings, model and data stay as they are'
    $model = @('--keep-model')
}
elseif ($Hosted) {
    Say 'Using a hosted model, from OpenRouter. The next step asks for its key, here'
    $model = @('--hosted', 'openrouter')
}
elseif ($ModelEndpoint) {
    Say "Using the model server at $ModelEndpoint"
    $model = @('--model-endpoint', $ModelEndpoint, '--model', $ModelName)
}
elseif ((Get-FromWsl $Distro @('curl', '-s', '-m', '3', 'http://localhost:11434/api/tags')) -match ('"name":"' + [regex]::Escape($ModelName) + '"')) {
    Say "Using the Ollama already running on this PC, which has $ModelName"
    $model = @('--model-endpoint', 'http://localhost:11434/v1', '--model', $ModelName)
}
else {
    if (-not (Get-FromWsl $Distro @('sh', '-c', 'command -v ollama'))) {
        Say 'A model on this PC needs Ollama, from ollama.com, which is someone else''s software'
        if ((Read-Host 'Install Ollama in this distro now? [y/N]') -notmatch '^[Yy]') {
            Stop-Setup 'Nothing was installed. Run this again to install Ollama, or choose a hosted model.'
        }
        & wsl.exe -d $Distro -u root --exec sh -c 'apt-get install -y -qq zstd > /dev/null 2>&1; curl -fsSL https://ollama.com/install.sh | sh'
        if ($LASTEXITCODE -ne 0) { Stop-Setup 'Ollama didn''t install. The lines above say why.' }
    }
    if ((Get-FromWsl $Distro @('sh', '-c', "ollama list | grep -c '^$ModelName'")) -eq '0') {
        Say "Downloading $ModelName, about 6.6 GB"
        & wsl.exe -d $Distro -u root --exec ollama pull $ModelName
        if ($LASTEXITCODE -ne 0) { Stop-Setup "$ModelName didn't download. The lines above say why." }
    }
    $model = @('--model-endpoint', 'http://localhost:11434/v1', '--model', $ModelName)
}

# Before the install: the broker reads this once, as it starts, to know how to
# speak here.
if ($Version) {
    & wsl.exe -d $Distro -u root --exec sh -c "mkdir -p /etc/hatchling && echo $Version > /etc/hatchling/windows-setup-version"
}
if (-not $updating) { Say 'Installing Hatchling' }
$install = @('-d', $Distro, '-u', 'root', '--exec', 'env', "SUDO_UID=$ownerUid", 'sh', '/root/hatchling-installer/scripts/install.sh', '--from', $from) + $model
if ($Hosted -and -not $updating) {
    # Left alone: it asks for a key, at a prompt that needs this console.
    & wsl.exe @install
}
else {
    # The app's address is a key, and the Start menu fetches it: it stays off
    # the screen, with what's only useful in a Linux shell.
    $linuxOnly = 'hatchling-ui=|^Open the app in a browser|^Each start of the broker|^\s*hatchling-host url|^A hosted model too|^which asks for its key|^After that, switch|^Hatchling is up to date, and locked|^\s*sudo hatchling-broker unlock'
    & wsl.exe @install | ForEach-Object { if ($_ -notmatch $linuxOnly) { $_ } }
}
if ($LASTEXITCODE -ne 0) { Stop-Setup 'Hatchling didn''t install. The lines above say why, and running this again carries on.' }
& wsl.exe -d $Distro -u root --exec rm -rf $from

Say 'Keeping it running after this window closes'
Start-KeepRunning $Distro
Write-Host ''
Write-Host 'Done. Open Hatchling from the Start menu.' -ForegroundColor Green
if ((Get-FromWsl $Distro @('sh', '-c', 'hatchling-broker vault-keyring status 2>/dev/null | grep -c "tier hardware key"')) -ne '0') {
    Write-Host 'It restarted, so it''s locked: Hatchling asks for your key when you open it.'
}
Write-Host ''
Read-Host 'Press Enter to close'
