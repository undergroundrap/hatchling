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
    if (-not $script:Unattended) { Read-Host 'Press Enter to close' }
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

# Send the exact bytes to WSL root. The distro's default user may be unable
# to write the private staging directory through the Windows file share.
function Copy-BundledRelease([string] $Distro, [string] $Release, [string] $Destination) {
    if ($Distro -notmatch '^[A-Za-z0-9._-]+$' -or $Destination -notmatch '^/root/hatchling-release\.[A-Za-z0-9]{8}$') {
        throw 'Invalid WSL release destination.'
    }
    # Windows PowerShell gives a child its input through a writer in the
    # console's input encoding, and that writer puts the encoding's byte-order
    # mark ahead of everything else. On a console set to UTF-8 every file
    # arrived three bytes longer and failed its checksum. For the copy, the
    # console's input is UTF-8 with no mark; it is put back after. Where it
    # can't be changed, the checksums still refuse what arrives changed.
    $console = [Console]::InputEncoding
    $marked = $console.GetPreamble().Length -gt 0
    if ($marked) {
        try { [Console]::InputEncoding = New-Object Text.UTF8Encoding $false }
        catch { $marked = $false }
    }
    try {
        Copy-ReleaseFiles $Distro $Release $Destination
    }
    finally {
        if ($marked) { [Console]::InputEncoding = $console }
    }
}

# Each file of the release, written by `dd` as root from this process's pipe.
function Copy-ReleaseFiles([string] $Distro, [string] $Release, [string] $Destination) {
    foreach ($file in Get-ChildItem -LiteralPath $Release -File -ErrorAction Stop) {
        if ($file.Name -notmatch '^[A-Za-z0-9._-]+$' -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Unexpected file in the bundled release: $($file.Name)"
        }
        $start = New-Object Diagnostics.ProcessStartInfo
        $start.FileName = Join-Path $env:WINDIR 'System32\wsl.exe'
        $start.Arguments = "-d $Distro -u root --exec /usr/bin/dd of=$Destination/$($file.Name) status=none"
        $start.UseShellExecute = $false
        $start.RedirectStandardInput = $true
        $start.CreateNoWindow = $true
        $inputFile = [IO.File]::OpenRead($file.FullName)
        try {
            $process = [Diagnostics.Process]::Start($start)
            try {
                $pipe = $process.StandardInput.BaseStream
                try { $inputFile.CopyTo($pipe) }
                finally {
                    $pipe.Dispose()
                    $process.WaitForExit()
                }
                if ($process.ExitCode -ne 0) { throw "WSL couldn't receive $($file.Name)." }
            }
            finally { $process.Dispose() }
        }
        finally { $inputFile.Dispose() }
    }
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
