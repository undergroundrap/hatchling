# Frees the space a WSL distro's disk file keeps after files are deleted in
# it (#520). The file (ext4.vhdx) grows to its peak use and never shrinks by
# itself: compacting it gives Windows back what's free inside.
#
#   compact.ps1 [-Distro hatchling-wsl] [-DryRun]
#
# As you, it checks nothing is busy, says what it will do, cleans Hatchling's
# leftovers (`hatchling-broker clean`, never your data) and stops Hatchling.
# Then it asks Windows to run its second half as an administrator: that half
# finds the distro's own disk file from your registry (never a path it's
# given), stops WSL and holds it stopped, compacts that one file, and puts
# WSL's service back as it was on every path.
param(
    [string] $Distro = 'hatchling-wsl',
    [switch] $DryRun,
    # The second half, run elevated by the first.
    [switch] $Elevated
)
# Not 'Stop': in Windows PowerShell, a native program's stderr is an error, and
# each step here checks its exit code instead.
$ErrorActionPreference = 'Continue'
. (Join-Path $PSScriptRoot 'common.ps1')

$script:Lxss = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss'
# The commands that change a distro's stores or its vault. While one runs,
# stopping WSL would cut it off halfway.
$script:BusyVerbs = @('backup', 'restore', 'reset', 'lock', 'rekey-vault', 'migrate-vault', 'vault-keyring', 'room-disk', 'clean')

function Say([string] $Words) { Write-Host "== $Words" -ForegroundColor Cyan }

# Bytes as Windows' File Explorer says them.
function Format-Size([long] $Bytes) {
    if ($Bytes -ge 1GB) { return '{0:N2} GB' -f ($Bytes / 1GB) }
    if ($Bytes -ge 1MB) { return '{0:N0} MB' -f ($Bytes / 1MB) }
    return "$Bytes bytes"
}

# The distros WSL keeps in your registry: each with its DistributionName and
# BasePath.
function Get-Registrations {
    return @(Get-ChildItem -LiteralPath $script:Lxss -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ItemProperty -LiteralPath $_.PSPath })
}

# The distro's own disk file, from its registration. Anything else (no
# registration, two, a link, a missing file) is an error that says what it
# found: nothing is compacted but this one file.
function Get-DistroDisk([string] $Distro, [object[]] $Registrations = (Get-Registrations)) {
    $found = @($Registrations | Where-Object { $_.DistributionName -eq $Distro })
    if ($found.Count -eq 0) { throw "WSL has no distro named $Distro in this account." }
    if ($found.Count -gt 1) { throw "WSL has $($found.Count) distros named $Distro in this account." }
    $base = [string] $found[0].BasePath
    if ($base.StartsWith('\\?\')) { $base = $base.Substring(4) }
    if (-not $base -or -not [IO.Path]::IsPathRooted($base) -or $base.StartsWith('\\')) {
        throw "WSL keeps $Distro somewhere this can't compact: '$base'."
    }
    $folder = Get-Item -LiteralPath $base -Force -ErrorAction SilentlyContinue
    if (-not $folder -or -not $folder.PSIsContainer) { throw "$Distro's folder isn't there: $base" }
    if ($folder.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "$Distro's folder is a link, so it isn't compacted: $base" }
    $disk = Get-Item -LiteralPath (Join-Path $base 'ext4.vhdx') -Force -ErrorAction SilentlyContinue
    if (-not $disk -or $disk.PSIsContainer) { throw "$Distro has no disk file ext4.vhdx in $base" }
    if ($disk.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "$Distro's disk file is a link, so it isn't compacted: $($disk.FullName)" }
    return $disk.FullName
}

# What's running that a compaction would cut off, from the distro's processes
# (`ps -eo args=`, one a line) and Windows' (their command lines): a backup,
# a restore or another of the broker's store commands, or Hatchling's setup.
# One reason a line; none when nothing is busy.
function Find-BusyWork([string] $Distro, [string[]] $LinuxArgs, [string[]] $WindowsLines) {
    $reasons = @()
    foreach ($line in $LinuxArgs) {
        $words = @("$line".Trim() -split '\s+')
        if ($words.Count -ge 2 -and $words[0] -match '(^|/)hatchling-broker$' -and $script:BusyVerbs -contains $words[1]) {
            $reasons += "hatchling-broker $($words[1]) is running in $Distro"
        }
    }
    foreach ($line in $WindowsLines) {
        # Inno Setup's: the setup, then its own copy as a .tmp.
        if ("$line" -match '(?i)(^|[\\"\s])Hatchling-[^\\"\s]*-Setup\.(exe|tmp)') {
            $reasons += "Hatchling's setup is running"
        }
        elseif ("$line" -match '(?i)[\\"\s]setup\.ps1') {
            # Without -Distro, it's the default distro's.
            $named = if ("$line" -match '(?i)-Distro\s+"?([A-Za-z0-9._-]+)') { $Matches[1] } else { 'hatchling-wsl' }
            if ($named -eq $Distro) { $reasons += "Hatchling's setup is updating $Distro" }
        }
    }
    return @($reasons | Select-Object -Unique)
}

# The space the distro's root file system has, and uses, in bytes.
function Get-InsideUse([string] $Distro) {
    $said = Get-FromWsl $Distro @('df', '-B1', '--output=size,used', '/')
    $numbers = @($said -split '\s+' | Where-Object { $_ -match '^\d+$' })
    if ($numbers.Count -ne 2) { return $null }
    return @{ Size = [long] $numbers[0]; Used = [long] $numbers[1] }
}

# ---- The elevated half: WSL's service, and the one disk file.

# How WslService starts: auto, delayed-auto, demand or disabled, as sc.exe
# names them; nothing when there's no such service.
function Get-WslStart {
    $service = Get-CimInstance Win32_Service -Filter "Name='WslService'" -ErrorAction SilentlyContinue
    if (-not $service) { return $null }
    switch ($service.StartMode) {
        'Auto' { if ($service.DelayedAutoStart) { return 'delayed-auto' } return 'auto' }
        'Manual' { return 'demand' }
        'Disabled' { return 'disabled' }
    }
    return $null
}
function Set-WslStart([string] $Mode) {
    & sc.exe config WslService start= $Mode > $null
    return $LASTEXITCODE -eq 0
}
function Test-WslRunning { return (Get-Service WslService -ErrorAction SilentlyContinue).Status -eq 'Running' }
function Stop-Wsl {
    & wsl.exe --shutdown > $null 2>&1
    Stop-Service WslService -Force -ErrorAction SilentlyContinue
    foreach ($try in 1..30) {
        if (-not (Test-WslRunning)) { return $true }
        Start-Sleep -Seconds 1
    }
    return $false
}
function Start-Wsl { Start-Service WslService -ErrorAction SilentlyContinue }
# Whether nothing has the file open: WSL lets go of it some time after it
# stops.
function Test-DiskFree([string] $Path) {
    foreach ($try in 1..30) {
        try {
            $stream = [IO.File]::Open($Path, 'Open', 'ReadWrite', 'None')
            $stream.Dispose()
            return $true
        }
        catch { Start-Sleep -Seconds 1 }
    }
    return $false
}
# Runs diskpart's commands from a file of its own; its exit code.
function Invoke-DiskPart([string[]] $Commands) {
    $file = Join-Path ([IO.Path]::GetTempPath()) "hatchling-compact-$([guid]::NewGuid().ToString('N')).txt"
    try {
        Set-Content -LiteralPath $file -Value $Commands -Encoding Ascii
        # To the console: in the pipeline, its lines would be returned with
        # the exit code.
        & diskpart.exe /s $file | Out-Host
        return $LASTEXITCODE
    }
    finally { Remove-Item -LiteralPath $file -Force -ErrorAction SilentlyContinue }
}

# Compacts `$Path` with WSL held stopped: its service is set not to start,
# stopped, and once nothing holds the file, diskpart compacts it. Whatever
# happens, the service's start is put back as it was, and started again if
# it was running. Returns what went wrong, or nothing.
function Compress-Disk([string] $Path) {
    $start = Get-WslStart
    if (-not $start) { return "WSL's service (WslService) isn't on this PC. Update WSL first: wsl --update" }
    $wasRunning = Test-WslRunning
    $attached = $false
    $wrong = $null
    try {
        # Without this, something starts WSL again within a second of every
        # shutdown, and diskpart finds the file in use.
        if (-not (Set-WslStart 'disabled')) { return "WSL's service couldn't be held stopped, so nothing was compacted." }
        if (-not (Stop-Wsl)) { return "WSL didn't stop, so nothing was compacted." }
        if (-not (Test-DiskFree $Path)) { return "Something still has the disk file open, so nothing was compacted: $Path" }
        $attached = $true
        $code = Invoke-DiskPart @("select vdisk file=`"$Path`"", 'attach vdisk readonly', 'compact vdisk', 'detach vdisk')
        $attached = $false
        if ($code -ne 0) { $wrong = "diskpart couldn't compact the disk file (exit $code). The lines above say why." }
    }
    finally {
        # A failed compact can leave the file attached, and WSL can't use it then.
        if ($attached -or $wrong) { Invoke-DiskPart @("select vdisk file=`"$Path`"", 'detach vdisk') | Out-Null }
        if (-not (Set-WslStart $start)) {
            Write-Host "WSL's service couldn't be set back to start as it did ($start). In an administrator PowerShell: sc.exe config WslService start= $start" -ForegroundColor Yellow
        }
        if ($wasRunning) { Start-Wsl }
    }
    return $wrong
}

function Invoke-ElevatedHalf([string] $Distro) {
    if ($Distro -notmatch '^[A-Za-z0-9._-]+$') { Stop-Setup "That isn't a distro's name: $Distro" }
    $admin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $admin) { Stop-Setup 'Compacting needs an administrator. Run Free up disk space from the Start menu.' }
    try { $path = Get-DistroDisk $Distro }
    catch { Stop-Setup "$_ Compacting needs an administrator's rights in the account that has the distro." }
    Say "Compacting $path"
    $wrong = Compress-Disk $path
    if ($wrong) { Stop-Setup $wrong }
    exit 0
}

# ---- The half that runs as you.

function Invoke-OwnerHalf([string] $Distro) {
    if ($Distro -notmatch '^[A-Za-z0-9._-]+$') { Stop-Setup "That isn't a distro's name: $Distro" }
    if ((Get-Distros) -notcontains $Distro) { Stop-Setup "WSL has no distro named $Distro." }
    try { $path = Get-DistroDisk $Distro }
    catch { Stop-Setup "$_" }

    $windows = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | ForEach-Object { $_.CommandLine } | Where-Object { $_ })
    $linux = @((Get-FromWsl $Distro @('ps', '-eo', 'args=')) -split "`n")
    $busy = @(Find-BusyWork $Distro $linux $windows)
    if ($busy.Count -gt 0) {
        Stop-Setup "Not now: $($busy -join '; '). Wait for it to finish, then run this again. Nothing was changed."
    }

    $before = (Get-Item -LiteralPath $path).Length
    $inside = Get-InsideUse $Distro
    Say "$Distro's disk file takes $(Format-Size $before) on Windows: $path"
    if ($inside) { Say "$(Format-Size $inside.Used) of it is in use inside. Compacting can give back about the rest." }
    $others = @(Get-Distros -Running | Where-Object { $_ -ne $Distro })
    # A distro without Hatchling (a test box, say) is compacted too, with
    # nothing of Hatchling's to clean, stop or start.
    $hatchling = (Get-FromWsl $Distro @('sh', '-c', 'test -x /usr/local/bin/hatchling-broker && echo yes')) -eq 'yes'
    Write-Host ''
    Write-Host 'This will:'
    if ($hatchling) {
        Write-Host "  - clean Hatchling's leftovers (hatchling-broker clean: never your data)"
        Write-Host "  - stop Hatchling, then every WSL distro$(if ($others.Count) { ", including $($others -join ', ')" }); Docker Desktop's WSL engine stops too"
    }
    else {
        Write-Host "  - stop every WSL distro$(if ($others.Count) { ", including $($others -join ', ')" }); Docker Desktop's WSL engine stops too. Hatchling isn't in $Distro, so there's nothing of it to clean"
    }
    Write-Host "  - ask Windows for an administrator, to compact $Distro's disk file. It holds WSL's service stopped meanwhile, and puts it back as it was"
    if ($hatchling) { Write-Host '  - start Hatchling again. A vault a security key protects is locked: open Hatchling from the Start menu to unlock it' }
    Write-Host ''
    if ($DryRun) {
        Say 'A dry run: nothing was changed.'
        return
    }
    if ((Read-Host 'Type yes to go on') -ne 'yes') { Stop-Setup 'Nothing was changed.' }

    if ($hatchling) {
        Say 'Stopping Hatchling'
        & wsl.exe -d $Distro -u root --exec systemctl stop hatchling
        if ($LASTEXITCODE -ne 0) { Stop-Setup "Hatchling didn't stop, so nothing was compacted. To see why: wsl -d $Distro -u root -- systemctl status hatchling" }
        Say "Cleaning Hatchling's leftovers"
        & wsl.exe -d $Distro -u root --exec hatchling-broker clean
        if ($LASTEXITCODE -ne 0) { Write-Host "The clean didn't finish (the lines above say why). Compacting goes on." -ForegroundColor Yellow }
    }

    Say 'Asking Windows for an administrator'
    $elevatedArgs = "-NoProfile -ExecutionPolicy RemoteSigned -File `"$PSCommandPath`" -Distro $Distro -Elevated"
    try { $half = Start-Process -FilePath (Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe') -ArgumentList $elevatedArgs -Verb RunAs -Wait -PassThru -ErrorAction Stop }
    catch { $half = $null }

    $started = $true
    if ($hatchling) {
        Say 'Starting Hatchling again'
        $started = Wait-Broker $Distro
        # The shutdown ended what kept Hatchling running, if Setup made one.
        if (Test-Path -LiteralPath (Join-Path ([Environment]::GetFolderPath('Startup')) 'Hatchling.lnk')) { Start-KeepRunning $Distro }
    }
    $after = (Get-Item -LiteralPath $path).Length
    if (-not $half) { Stop-Setup "Windows didn't run the administrator half, so nothing was compacted.$(if ($hatchling) { if ($started) { ' Hatchling is running again.' } else { " Hatchling didn't start: open it from the Start menu." } })" }
    if ($half.ExitCode -ne 0) { Stop-Setup "The disk file wasn't compacted: the administrator window said why. It still takes $(Format-Size $after)." }
    Say "Compacted: $(Format-Size $before) before, $(Format-Size $after) now."
    if (-not $started) { Write-Host "Hatchling didn't start again. Open it from the Start menu." -ForegroundColor Yellow }
    Read-Host 'Press Enter to close'
}

# Dot-sourced by the tests, this defines the functions and runs nothing.
if ($MyInvocation.InvocationName -ne '.') {
    if ($Elevated) { Invoke-ElevatedHalf $Distro }
    else { Invoke-OwnerHalf $Distro }
}
