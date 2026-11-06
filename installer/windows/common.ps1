# What the Windows scripts share. WSL is the machine Hatchling runs on: these
# only start it and ask it things. Nothing here sees a key, a PIN or a code:
# those are typed at the broker's own prompt, in a console the owner can see.

# Always --exec: `wsl -- <command>` goes through a shell, which eats a Windows
# path's backslashes.
$script:Owner = 'owner'

function Stop-Setup([string] $Message) {
    Write-Host ''
    Write-Host $Message -ForegroundColor Yellow
    Write-Host ''
    Read-Host 'Press Enter to close'
    exit 1
}

# The distros here, one name a line. wsl.exe writes UTF-16 to a pipe.
function Get-Distros {
    $before = [Console]::OutputEncoding
    [Console]::OutputEncoding = [System.Text.Encoding]::Unicode
    try {
        $names = & wsl.exe --list --quiet 2>$null
        if ($LASTEXITCODE -ne 0) { return @() }
        return @($names | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    }
    finally {
        [Console]::OutputEncoding = $before
    }
}

# A word from the distro, as root, without its line ending.
function Get-FromWsl([string] $Distro, [string[]] $Command) {
    $said = & wsl.exe -d $Distro -u root --exec @Command 2>$null
    if ($null -eq $said) { return '' }
    return (@($said) -join "`n").Trim()
}

# Waits for the broker to finish starting, so its address is this start's.
# True once it's up; false if it hasn't come up in time.
function Wait-Broker([string] $Distro) {
    foreach ($try in 1..30) {
        if ((Get-FromWsl $Distro @('systemctl', 'is-active', 'hatchling')) -eq 'active') { return $true }
        Start-Sleep -Seconds 1
    }
    return $false
}

# One idle process in WSL, with no window: without one, WSL stops the distro
# about 30 seconds after the last terminal closes, and Hatchling with it.
function Start-KeepRunning([string] $Distro) {
    $held = @(Get-CimInstance Win32_Process -Filter "name='wsl.exe'" | Where-Object { $_.CommandLine -match "-d $([regex]::Escape($Distro)) --exec sleep infinity" })
    if ($held.Count -gt 0) { return }
    Start-Process -FilePath "$env:WINDIR\System32\conhost.exe" -ArgumentList "--headless wsl.exe -d $Distro --exec sleep infinity"
}
