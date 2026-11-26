# Tests for common.ps1, with no WSL. Run in Windows PowerShell, as the Start
# menu runs the scripts:
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File installer\windows\tests\common.tests.ps1
#
# Exits 1 when any test fails.

. (Join-Path $PSScriptRoot '..\common.ps1')
$ErrorActionPreference = 'Stop'

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
    if ("$Expected" -ne "$Actual") { throw "$What`n  expected: $Expected`n  actual:   $Actual" }
}

$startup = Join-Path ([IO.Path]::GetTempPath()) "hatchling-test-startup-$PID"
Remove-Item -LiteralPath $startup -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $startup | Out-Null
try {
    Test-Case 'keep-running is off with no Startup shortcut' {
        Assert-Same 'off' (Get-KeepRunningSaid $startup)
    }
    Test-Case 'keep-running is on with the shortcut Setup makes' {
        New-Item -ItemType File -Path (Join-Path $startup 'Hatchling.lnk') | Out-Null
        Assert-Same 'on' (Get-KeepRunningSaid $startup)
    }
    Test-Case 'another program''s shortcut is not Hatchling''s' {
        Remove-Item -LiteralPath (Join-Path $startup 'Hatchling.lnk')
        New-Item -ItemType File -Path (Join-Path $startup 'Other.lnk') | Out-Null
        Assert-Same 'off' (Get-KeepRunningSaid $startup)
    }
}
finally {
    Remove-Item -LiteralPath $startup -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "$script:passed passed, $script:failed failed"
if ($script:failed -gt 0) { exit 1 }
