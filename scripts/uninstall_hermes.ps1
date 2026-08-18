# PowerShell Uninstaller for OB2H in Hermes (Windows)
param(
    [string]$HermesConfigPath = "C:\Users\ipres\AppData\Local\hermes\config.yaml"
)

$ErrorActionPreference = "Stop"

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

Write-Host "=== Uninstalling OB2H from Hermes ===" -ForegroundColor Cyan

if (-not (Test-Path $HermesConfigPath)) {
    Write-Error "Hermes config not found: $HermesConfigPath"
    exit 1
}

$BackupPath = "$HermesConfigPath.bak"
Copy-Item -Path $HermesConfigPath -Destination $BackupPath -Force
Write-Host "Backup created: $BackupPath" -ForegroundColor Gray

$rawContent = [System.IO.File]::ReadAllText($HermesConfigPath, [System.Text.Encoding]::UTF8)
$lines = $rawContent -split "\r?\n"
$filteredLines = @()
$inOb2h = $false

foreach ($line in $lines) {
    if ($line.StartsWith('  ob2h:') -or $line.StartsWith('  "ob2h":') -or $line.StartsWith("  'ob2h':")) {
        $inOb2h = $true
        continue
    }
    if ($inOb2h) {
        if ($line.StartsWith('    ') -or [string]::IsNullOrWhiteSpace($line)) {
            continue
        } else {
            $inOb2h = $false
        }
    }
    $filteredLines += $line
}

$finalContent = ($filteredLines -join "`r`n").TrimEnd()
if ($finalContent.EndsWith("mcp_servers:")) {
    $finalContent = $finalContent.Substring(0, $finalContent.Length - 12).TrimEnd()
}
$finalContent += "`r`n"

[System.IO.File]::WriteAllText($HermesConfigPath, $finalContent, [System.Text.Encoding]::UTF8)

Write-Host "OB2H successfully removed from $HermesConfigPath" -ForegroundColor Green
