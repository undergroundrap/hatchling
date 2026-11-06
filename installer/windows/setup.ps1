# Sets Hatchling up in a WSL distro of its own, from the release files beside
# this script. Run by the installer in a console the owner can see, or by hand:
#
#   setup.ps1 [-Distro Hatchling] [-Release <dir>] [-Hosted | -ModelEndpoint <url> -ModelName <name>]
#
# With neither, the model runs on this machine, with Ollama: one already running
# that has the model is used, and otherwise it asks before installing it. -Hosted uses OpenRouter, and the broker asks for its key
# at its own prompt in this console. -ModelEndpoint is a model server already
# running, at an address WSL can reach as localhost.
#
# The owner's account in the distro has no password and no sudo: only Windows'
# `wsl -u root` runs the commands that need root, so the app's host, which
# runs as that account, gets no way to root.
param(
    [string] $Distro = 'Hatchling',
    [string] $Release = (Join-Path $PSScriptRoot 'release'),
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

& wsl.exe -d $Distro -u root --exec sh -c "id -u $Owner > /dev/null 2>&1 || useradd -m -s /bin/bash $Owner"
if ($LASTEXITCODE -ne 0) { Stop-Setup "Couldn't make the account '$Owner' in $Distro." }
$ownerUid = Get-FromWsl $Distro @('id', '-u', $Owner)

Say 'Checking every file against the release''s SHA256SUMS'
$from = Get-FromWsl $Distro @('wslpath', '-a', $Release)
# No double quotes in this: Windows PowerShell mangles them on the way to a
# program, and --cd takes a path with spaces in it, such as a user's name.
$unpack = 'set -e; sha256sum --check --ignore-missing --quiet SHA256SUMS; grep -q hatchling-installer.tar.gz SHA256SUMS; rm -rf /root/hatchling-installer; mkdir -m 700 /root/hatchling-installer; tar -xzf hatchling-installer.tar.gz -C /root/hatchling-installer --no-same-owner --no-same-permissions'
& wsl.exe -d $Distro -u root --cd $Release --exec sh -c $unpack
if ($LASTEXITCODE -ne 0) { Stop-Setup 'A file differs from the release''s SHA256SUMS: nothing was installed.' }

$model = @()
if ($Hosted) {
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

Say 'Installing Hatchling'
& wsl.exe -d $Distro -u root --exec env "SUDO_UID=$ownerUid" sh /root/hatchling-installer/scripts/install.sh --from $from @model
if ($LASTEXITCODE -ne 0) { Stop-Setup 'Hatchling didn''t install. The lines above say why, and running this again carries on.' }

Say 'Keeping it running after this window closes'
Start-KeepRunning $Distro
Write-Host ''
Write-Host 'Done. Open Hatchling from the Start menu. Each time, that gets the app''s current address for you.' -ForegroundColor Green
Write-Host ''
Read-Host 'Press Enter to close'
