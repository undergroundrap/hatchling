# Proves the Windows setup copies a release's files into WSL byte for byte,
# whatever the console's input code page is. It makes a small release of
# its own (a megabyte of bytes, an empty file, a short text), copies it into
# a private directory in the distro with the setup's own function, once
# with the console's input set to UTF-8 and once to code page 437, and
# compares the SHA-256 of what arrived with what was sent.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows-release-copy-proof.ps1 -Distro <a WSL distro>
#
# It needs Windows with WSL and a distro it may write a directory of root's
# in, which it removes. It exits 0 when every file arrived the same.
param([Parameter(Mandatory = $true)][string] $Distro)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\installer\windows\common.ps1')

$before = [Console]::InputEncoding
$release = Join-Path $env:TEMP ('hatchling-copy-proof-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $release | Out-Null
$different = 0
try {
    $bytes = New-Object byte[] 1048576
    (New-Object Random 7).NextBytes($bytes)
    [IO.File]::WriteAllBytes((Join-Path $release 'random.bin'), $bytes)
    [IO.File]::WriteAllBytes((Join-Path $release 'empty.bin'), (New-Object byte[] 0))
    [IO.File]::WriteAllText((Join-Path $release 'text.txt'), "a short text`n", (New-Object Text.UTF8Encoding $false))
    foreach ($page in 65001, 437) {
        [Console]::InputEncoding = [Text.Encoding]::GetEncoding($page)
        $destination = Get-FromWsl $Distro @('mktemp', '-d', '/root/hatchling-release.XXXXXXXX')
        if ($destination -notmatch '^/root/hatchling-release\.[A-Za-z0-9]{8}$') { throw "Couldn't make a private directory in $Distro." }
        try {
            Copy-BundledRelease $Distro $release $destination
            $arrived = @(& wsl.exe -d $Distro -u root --exec /bin/sh -c "cd $destination && sha256sum *")
            foreach ($file in Get-ChildItem -LiteralPath $release -File) {
                $sent = (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash.ToLowerInvariant()
                $line = $arrived | Where-Object { $_ -match ('  ' + [regex]::Escape($file.Name) + '$') }
                $got = if ($line) { ($line -split ' ')[0] } else { 'missing' }
                $same = $sent -eq $got
                if (-not $same) { $different++ }
                '{0,6}  {1,-10} {2,8} bytes  {3}' -f $page, $file.Name, $file.Length, $(if ($same) { 'the same' } else { 'DIFFERENT' })
            }
            $after = [Console]::InputEncoding
            if ($after.CodePage -ne $page -or $after.GetPreamble().Length -ne [Text.Encoding]::GetEncoding($page).GetPreamble().Length) {
                $different++
                "{0,6}  the console's input encoding was left changed" -f $page
            }
        }
        finally {
            & wsl.exe -d $Distro -u root --exec /bin/rm -rf $destination
        }
    }
}
finally {
    [Console]::InputEncoding = $before
    Remove-Item -Recurse -Force -LiteralPath $release
}
if ($different -ne 0) {
    "$different differ: the copy changes what it carries"
    exit 1
}
'every file arrived the same, on both code pages'
