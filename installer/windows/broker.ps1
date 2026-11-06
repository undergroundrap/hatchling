# Runs one of the broker's guided commands in this console, for the Start menu:
# what's secret is typed at its own prompt here, and never passes through this
# script.
param(
    [string] $Distro = 'hatchling-wsl',
    [ValidateSet('protect', 'backup', 'unlock')] [string] $Do
)
# Not 'Stop': in Windows PowerShell, a native program's stderr is an error, and
# each step here checks wsl.exe's exit code instead.
$ErrorActionPreference = 'Continue'
. (Join-Path $PSScriptRoot 'common.ps1')

# Typed, or a lone word stops being a list, and splatting takes it apart.
[string[]] $command = switch ($Do) {
    'protect' { @('vault-keyring', 'setup') }
    'backup' { @('backup') }
    'unlock' { @('unlock') }
}
if (-not (Wait-Broker $Distro)) {
    Stop-Setup "Hatchling didn't start. To see why, in PowerShell: wsl -d $Distro -u root -- hatchling-broker doctor"
}
& wsl.exe -d $Distro -u root --exec hatchling-broker @command
Write-Host ''
Read-Host 'Press Enter to close'
