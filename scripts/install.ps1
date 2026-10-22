# Installs or updates Tasca for the current user. Rerun it to update.
#   release -> quit the running resident -> copy -> Startup shortcut -> start hidden
#   source  -> build -> quit the running resident -> copy -> Startup shortcut -> start hidden
# A tasca.exe next to this script (an unzipped release) is installed as is, with no build.
$ErrorActionPreference = 'Stop'

$prebuilt = Join-Path $PSScriptRoot 'tasca.exe'
$repo = Split-Path -Parent $PSScriptRoot
$built = Join-Path $repo 'target\release\tasca.exe'
$dir = Join-Path $env:LOCALAPPDATA 'Tasca'
$exe = Join-Path $dir 'tasca.exe'
$lnk = Join-Path ([Environment]::GetFolderPath('Startup')) 'Tasca.lnk'

if (Test-Path -LiteralPath $prebuilt) {
    Write-Host "Using the release build next to this script (no build)..."
    $source = $prebuilt
} else {
    Write-Host 'Building (release)...'
    Push-Location $repo
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed (exit $LASTEXITCODE)" }
    } finally { Pop-Location }
    $source = $built
}

# The spike registers the same window class, so it must go first (it has no quit verb).
Get-Process -Name 'tasca-spike' -ErrorAction SilentlyContinue | Stop-Process -Force

function Get-Installed {
    Get-Process -Name 'tasca' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $exe }
}

$running = @(Get-Installed)
if ($running.Count -gt 0) {
    Write-Host "Quitting running Tasca (PID $($running.Id -join ', '))..."
    & $exe quit
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (@(Get-Installed).Count -gt 0 -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
    if (@(Get-Installed).Count -gt 0) {
        throw "Tasca (PID $((Get-Installed).Id -join ', ')) did not exit within 10 s after 'tasca.exe quit'. Close it by hand and rerun."
    }
}

New-Item -ItemType Directory -Force -Path $dir | Out-Null
Copy-Item -LiteralPath $source -Destination $exe -Force
# A downloaded exe carries Mark-of-the-Web; drop it from the installed copy.
Unblock-File -LiteralPath $exe -ErrorAction SilentlyContinue
Write-Host "Installed $exe"

$sh = New-Object -ComObject WScript.Shell
$sc = $sh.CreateShortcut($lnk)
$sc.TargetPath = $exe
$sc.Arguments = '--hidden'
$sc.WorkingDirectory = $dir
$sc.Save()
Write-Host "Startup shortcut: $lnk"

Start-Process -FilePath $exe -ArgumentList '--hidden'
Write-Host 'Started tasca.exe --hidden'
