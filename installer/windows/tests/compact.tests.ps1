# Tests for compact.ps1 (#520), with no WSL, no service and no diskpart: what
# it would change is played by stand-ins that record each call. Run in
# Windows PowerShell, as the Start menu runs the script:
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File installer\windows\tests\compact.tests.ps1
#
# Exits 1 when any test fails.

# Dot-sourced, compact.ps1 defines its functions and runs nothing.
. (Join-Path $PSScriptRoot '..\compact.ps1')
$ErrorActionPreference = 'Stop'
[Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::InvariantCulture

$script:passed = 0
$script:failed = 0
function Test-Case([string] $Name, [scriptblock] $Body) {
    try {
        & $Body
        $script:passed++
        Write-Host "ok - $Name"
    }
    catch {
        $script:failed++
        Write-Host "not ok - ${Name}: $_" -ForegroundColor Red
    }
}
function Assert-Same($Expected, $Actual, [string] $What = 'value') {
    $want = ($Expected | ForEach-Object { "$_" }) -join "`n"
    $have = ($Actual | ForEach-Object { "$_" }) -join "`n"
    if ($want -ne $have) { throw "$What`n  expected: $want`n  actual:   $have" }
}
function Assert-Throws([scriptblock] $Body, [string] $Pattern) {
    try { & $Body }
    catch {
        if ("$_" -notmatch $Pattern) { throw "threw '$_', not /$Pattern/" }
        return
    }
    throw "didn't throw /$Pattern/"
}

$scratch = Join-Path ([IO.Path]::GetTempPath()) "hatchling-compact-test-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    # ---- Sizes

    Test-Case 'sizes are said as File Explorer says them' {
        Assert-Same '19.97 GB' (Format-Size ([long] (19.97 * 1GB)))
        Assert-Same '1.00 GB' (Format-Size 1GB)
        Assert-Same '512 MB' (Format-Size 512MB)
        Assert-Same '42 bytes' (Format-Size 42)
    }

    # ---- The disk file, from the registration only

    $home1 = Join-Path $scratch 'distro'
    New-Item -ItemType Directory -Path $home1 | Out-Null
    Set-Content -LiteralPath (Join-Path $home1 'ext4.vhdx') -Value 'disk'
    $disk = (Get-Item -LiteralPath (Join-Path $home1 'ext4.vhdx')).FullName
    function Registration([string] $Name, [string] $Base) { [pscustomobject] @{ DistributionName = $Name; BasePath = $Base } }

    Test-Case "the disk file is the registration's own, with or without \\?\" {
        Assert-Same $disk (Get-DistroDisk 'hatchling-wsl' @((Registration 'Ubuntu' 'C:\nowhere'), (Registration 'hatchling-wsl' $home1)))
        Assert-Same $disk (Get-DistroDisk 'hatchling-wsl' @(Registration 'hatchling-wsl' "\\?\$home1"))
    }
    Test-Case 'no registration, or two, is refused' {
        Assert-Throws { Get-DistroDisk 'hatchling-wsl' @(Registration 'Ubuntu' $home1) } 'no distro named hatchling-wsl'
        Assert-Throws { Get-DistroDisk 'hatchling-wsl' @((Registration 'hatchling-wsl' $home1), (Registration 'hatchling-wsl' $home1)) } 'has 2 distros named'
    }
    Test-Case "a folder that isn't a local absolute path is refused" {
        foreach ($base in @('', 'relative\folder', '\\server\share\distro', '\\?\UNC\server\share\distro')) {
            Assert-Throws { Get-DistroDisk 'd' @(Registration 'd' $base) } "somewhere this can't compact"
        }
    }
    Test-Case 'a missing folder or disk file is refused' {
        Assert-Throws { Get-DistroDisk 'd' @(Registration 'd' (Join-Path $scratch 'gone')) } "folder isn't there"
        $empty = Join-Path $scratch 'empty'
        New-Item -ItemType Directory -Path $empty | Out-Null
        Assert-Throws { Get-DistroDisk 'd' @(Registration 'd' $empty) } 'no disk file ext4.vhdx'
        New-Item -ItemType Directory -Path (Join-Path $empty 'ext4.vhdx') | Out-Null
        Assert-Throws { Get-DistroDisk 'd' @(Registration 'd' $empty) } 'no disk file ext4.vhdx'
    }
    Test-Case "a folder that's a link is refused: it could lead to any disk file" {
        $link = Join-Path $scratch 'link'
        New-Item -ItemType Junction -Path $link -Target $home1 | Out-Null
        Assert-Throws { Get-DistroDisk 'd' @(Registration 'd' $link) } 'folder is a link'
    }

    # ---- What's busy

    Test-Case "the broker's store commands are busy, and nothing else of it is" {
        $linux = @(
            '/usr/local/bin/hatchling-broker',
            '/usr/local/bin/hatchling-broker backup /root/hatchling.backup',
            'hatchling-broker restore /root/hatchling.backup',
            'hatchling-broker doctor',
            'sudo hatchling-broker vault-keyring setup',
            'vim hatchling-broker backup',
            '/usr/local/bin/hatchling-broker room-disk grow 20G',
            ''
        )
        Assert-Same @('hatchling-broker backup is running in d', 'hatchling-broker restore is running in d', 'hatchling-broker room-disk is running in d') (Find-BusyWork 'd' $linux @())
        Assert-Same @() (Find-BusyWork 'd' @('/usr/local/bin/hatchling-broker', 'sleep infinity') @())
    }
    Test-Case "Hatchling's setup is busy: Inno's, and setup.ps1 for this distro" {
        Assert-Same @("Hatchling's setup is running") (Find-BusyWork 'hatchling-wsl' @() @('"C:\Users\o\Downloads\Hatchling-0.9.0-Setup.exe"'))
        Assert-Same @("Hatchling's setup is running") (Find-BusyWork 'hatchling-wsl' @() @('"C:\Users\o\AppData\Local\Temp\is-AB12C.tmp\Hatchling-0.9.0-Setup.tmp" /SL5="$1"'))
        Assert-Same @("Hatchling's setup is running") (Find-BusyWork 'hatchling-wsl' @() @('Hatchling-0.9.0-Setup.exe /SILENT'))
        $setup = 'powershell.exe -NoProfile -ExecutionPolicy RemoteSigned -File "C:\Users\o\AppData\Local\Temp\is-X.tmp\setup.ps1" -Distro hatchling-wsl -Version 0.9.0'
        Assert-Same @("Hatchling's setup is updating hatchling-wsl") (Find-BusyWork 'hatchling-wsl' @() @($setup))
        Assert-Same @() (Find-BusyWork 'other' @() @($setup))
        Assert-Same @("Hatchling's setup is updating hatchling-wsl") (Find-BusyWork 'hatchling-wsl' @() @('powershell -File C:\x\setup.ps1'))
        Assert-Same @() (Find-BusyWork 'hatchling-wsl' @() @('powershell -File "C:\x\compact.ps1" -Distro hatchling-wsl', 'C:\x\notsetup.ps1', 'notepad.exe'))
    }
    Test-Case 'each reason is said once' {
        Assert-Same @("Hatchling's setup is running") (Find-BusyWork 'd' @() @('Hatchling-1-Setup.exe', 'C:\t\Hatchling-1-Setup.tmp'))
    }

    # ---- How WSL's service starts

    Test-Case "WslService's start is read as sc.exe names it" {
        $cases = @(
            @{ Service = [pscustomobject] @{ StartMode = 'Auto'; DelayedAutoStart = $true }; Want = 'delayed-auto' },
            @{ Service = [pscustomobject] @{ StartMode = 'Auto'; DelayedAutoStart = $false }; Want = 'auto' },
            @{ Service = [pscustomobject] @{ StartMode = 'Manual'; DelayedAutoStart = $false }; Want = 'demand' },
            @{ Service = [pscustomobject] @{ StartMode = 'Disabled'; DelayedAutoStart = $false }; Want = 'disabled' },
            @{ Service = [pscustomobject] @{ StartMode = 'Boot'; DelayedAutoStart = $false }; Want = $null },
            @{ Service = $null; Want = $null }
        )
        try {
            foreach ($case in $cases) {
                $script:fakeService = $case.Service
                function script:Get-CimInstance([string] $ClassName, [string] $Filter, $ErrorAction) {
                    if ($ClassName -ne 'Win32_Service' -or $Filter -ne "Name='WslService'") { throw "asked for $ClassName $Filter" }
                    return $script:fakeService
                }
                Assert-Same $case.Want (Get-WslStart) "start of $($case.Service.StartMode)"
            }
        }
        finally { Remove-Item function:script:Get-CimInstance -ErrorAction SilentlyContinue }
    }

    # ---- Compacting, with stand-ins for the service, WSL and diskpart

    $script:log = New-Object Collections.ArrayList
    function Use-Plan([hashtable] $Plan = @{}) {
        $script:log.Clear()
        $script:plan = @{ Start = 'auto'; Running = $true; Disable = $true; Restore = $true; Stop = $true; Free = $true; Compact = 0; Throws = $false }
        foreach ($key in $Plan.Keys) { $script:plan[$key] = $Plan[$key] }
    }
    function script:Get-WslStart { return $script:plan.Start }
    function script:Set-WslStart([string] $Mode) {
        [void] $script:log.Add("start=$Mode")
        if ($Mode -eq 'disabled') { return $script:plan.Disable }
        return $script:plan.Restore
    }
    function script:Test-WslRunning { return $script:plan.Running }
    function script:Stop-Wsl { [void] $script:log.Add('stop'); return $script:plan.Stop }
    function script:Start-Wsl { [void] $script:log.Add('start') }
    function script:Test-DiskFree([string] $Path) { [void] $script:log.Add("free? $Path"); return $script:plan.Free }
    function script:Invoke-DiskPart([string[]] $Commands) {
        [void] $script:log.Add("diskpart: $($Commands -join '; ')")
        if ($Commands -contains 'compact vdisk') {
            if ($script:plan.Throws) { throw 'diskpart went away' }
            return $script:plan.Compact
        }
        return 0
    }
    $target = 'C:\Users\o\AppData\Local\wsl\{guid}\ext4.vhdx'
    $compact = "diskpart: select vdisk file=`"$target`"; attach vdisk readonly; compact vdisk; detach vdisk"
    $detach = "diskpart: select vdisk file=`"$target`"; detach vdisk"

    Test-Case 'it compacts with WSL held stopped, then puts the service back and starts it' {
        Use-Plan
        Assert-Same $null (Compress-Disk $target) 'what went wrong'
        Assert-Same @('start=disabled', 'stop', "free? $target", $compact, 'start=auto', 'start') $script:log 'the calls'
    }
    Test-Case 'a delayed start is put back delayed, and a stopped service stays stopped' {
        Use-Plan @{ Start = 'delayed-auto'; Running = $false }
        Assert-Same $null (Compress-Disk $target)
        Assert-Same @('start=disabled', 'stop', "free? $target", $compact, 'start=delayed-auto') $script:log
    }
    Test-Case 'with no WslService, nothing is touched' {
        Use-Plan @{ Start = $null }
        Assert-Same "WSL's service (WslService) isn't on this PC. Update WSL first: wsl --update" (Compress-Disk $target)
        Assert-Same @() $script:log
    }
    Test-Case "a service that can't be held stopped is put back, and nothing is compacted" {
        Use-Plan @{ Disable = $false }
        Assert-Same "WSL's service couldn't be held stopped, so nothing was compacted." (Compress-Disk $target)
        Assert-Same @('start=disabled', 'start=auto', 'start') $script:log
    }
    Test-Case "WSL that won't stop: the service is put back, nothing is compacted" {
        Use-Plan @{ Stop = $false }
        Assert-Same "WSL didn't stop, so nothing was compacted." (Compress-Disk $target)
        Assert-Same @('start=disabled', 'stop', 'start=auto', 'start') $script:log
    }
    Test-Case 'a disk file still open: the service is put back, nothing is compacted' {
        Use-Plan @{ Free = $false }
        Assert-Same "Something still has the disk file open, so nothing was compacted: $target" (Compress-Disk $target)
        Assert-Same @('start=disabled', 'stop', "free? $target", 'start=auto', 'start') $script:log
    }
    Test-Case 'a failed compaction detaches the file, and the service is put back' {
        Use-Plan @{ Compact = 87 }
        Assert-Same "diskpart couldn't compact the disk file (exit 87). The lines above say why." (Compress-Disk $target)
        Assert-Same @('start=disabled', 'stop', "free? $target", $compact, $detach, 'start=auto', 'start') $script:log
    }
    Test-Case 'diskpart failing outright still detaches the file and puts the service back' {
        Use-Plan @{ Throws = $true }
        Assert-Throws { Compress-Disk $target } 'diskpart went away'
        Assert-Same @('start=disabled', 'stop', "free? $target", $compact, $detach, 'start=auto', 'start') $script:log
    }
    Test-Case "a service that can't be put back is said, with the command that does" {
        Use-Plan @{ Restore = $false }
        $said = & { Compress-Disk $target } 6>&1 | ForEach-Object { "$_" }
        if (-not ($said -match [regex]::Escape('sc.exe config WslService start= auto'))) { throw "not said: $said" }
        Assert-Same @('start=disabled', 'stop', "free? $target", $compact, 'start=auto', 'start') $script:log
    }
}
finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host "$script:passed passed, $script:failed failed"
if ($script:failed -gt 0) { exit 1 }
exit 0
