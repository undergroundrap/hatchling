# Opens the app at its current address: each start of the broker makes a new
# one, so a saved link stops working on purpose. This asks for it each time.
# A vault that a security key protects is unlocked here first, at the broker's
# own prompt.
param([string] $Distro = 'hatchling-wsl')
# Not 'Stop': in Windows PowerShell, a native program's stderr is an error, and
# each step here checks wsl.exe's exit code instead.
$ErrorActionPreference = 'Continue'
. (Join-Path $PSScriptRoot 'common.ps1')

if (-not (Wait-Broker $Distro)) {
    Stop-Setup "Hatchling didn't start. To see why, in PowerShell: wsl -d $Distro -u root -- hatchling-broker doctor"
}
$locked = Get-FromWsl $Distro @('curl', '-s', '-m', '3', 'http://127.0.0.1:7340/hatchling-locked')
if ($locked -match '"locked":true') {
    & wsl.exe -d $Distro -u root --exec hatchling-broker unlock
    if ($LASTEXITCODE -ne 0) { Stop-Setup 'The vault is still locked. Run Open Hatchling again to try once more.' }
    if (-not (Wait-Broker $Distro)) { Stop-Setup "Hatchling didn't start after unlocking." }
}
$address = (& wsl.exe -d $Distro -u $Owner --exec hatchling-host url 2>$null | Select-Object -First 1)
if ($address -notmatch '^http://127\.0\.0\.1:\d+/#hatchling-ui=[0-9a-f]+$') {
    Stop-Setup "Hatchling's address couldn't be read. To see why, in PowerShell: wsl -d $Distro -u root -- hatchling-broker doctor"
}
Start-Process $address
