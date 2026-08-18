# PowerShell Installer for OB2H in Hermes (Windows)
param(
    [string]$HermesConfigPath = "C:\Users\ipres\AppData\Local\hermes\config.yaml",
    [string]$ProjectDir = "C:\Projects\omnesbot_for_hermes"
)

$ErrorActionPreference = "Stop"

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

Write-Host "=== Installing OB2H (Rust) into Hermes ===" -ForegroundColor Cyan

$ExePath = Join-Path $ProjectDir "target\release\ob2h.exe"
if (-not (Test-Path $ExePath)) {
    Write-Host "Building release binary ob2h.exe..." -ForegroundColor Yellow
    Set-Location $ProjectDir
    & "C:\Users\ipres\.cargo\bin\cargo.exe" build --release
    if (-not (Test-Path $ExePath)) {
        Write-Error "Failed to build $ExePath"
        exit 1
    }
}

Write-Host "Binary ready: $ExePath" -ForegroundColor Green

if (-not (Test-Path $HermesConfigPath)) {
    Write-Error "Hermes config not found: $HermesConfigPath"
    exit 1
}

$BackupPath = "$HermesConfigPath.bak"
Copy-Item -Path $HermesConfigPath -Destination $BackupPath -Force
Write-Host "Backup created: $BackupPath" -ForegroundColor Gray

$rawContent = [System.IO.File]::ReadAllText($HermesConfigPath, [System.Text.Encoding]::UTF8)

# Remove existing ob2h block
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

$hasMcpServers = $false
foreach ($l in $filteredLines) {
    if ($l.Trim() -eq 'mcp_servers:') {
        $hasMcpServers = $true
        break
    }
}

$EscapedExe = $ExePath.Replace('\', '/')
$EscapedData = (Join-Path $ProjectDir 'data').Replace('\', '/')

$ob2hBlock = @"
  ob2h:
    command: "$EscapedExe"
    args:
      - "serve"
    env:
      OB2H_DATA_DIR: "$EscapedData"
      OB2H_LLM_BASE_URL: "https://api.deepseek.com/v1"
      OB2H_LLM_API_KEY: "DEEPSEEK_API_KEY"
      OB2H_LLM_MODEL: "deepseek-v4-flash"
      OB2H_EMBED_PROVIDER: "local"
      OB2H_AUTODREAM_ENABLED: "true"
"@

$finalContent = ""
if ($hasMcpServers) {
    $outList = @()
    foreach ($l in $filteredLines) {
        $outList += $l
        if ($l.Trim() -eq 'mcp_servers:') {
            $outList += ($ob2hBlock -split "\r?\n")
        }
    }
    $finalContent = ($outList -join "`r`n")
} else {
    $finalContent = ($filteredLines -join "`r`n").TrimEnd() + "`r`n`r`nmcp_servers:`r`n" + $ob2hBlock + "`r`n"
}

[System.IO.File]::WriteAllText($HermesConfigPath, $finalContent, [System.Text.Encoding]::UTF8)

Write-Host "OB2H successfully registered in Hermes ($HermesConfigPath)!" -ForegroundColor Green
Write-Host "Embedded model (Candle / paraphrase-multilingual-MiniLM-L12-v2) ready out of the box." -ForegroundColor Cyan
