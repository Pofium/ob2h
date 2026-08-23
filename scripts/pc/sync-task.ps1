# Синхронизация ob2h PC <-> VPS (push + pull по SSH-алиасу vps-alt).
# Запуск вручную: powershell -File sync-task.ps1
# Регистрация в планировщике (ежедневно 05:30, после дрима):
#   powershell -File sync-task.ps1 -Register

param(
    [switch]$Register,
    [string]$Binary = "C:\Projects\omnesbot_for_hermes\target\release\ob2h.exe"
)

$env:OB2H_DATA_DIR = "C:\Projects\omnesbot_for_hermes\data"

if ($Register) {
    $action = New-ScheduledTaskAction -Execute "powershell.exe" `
        -Argument "-NoProfile -WindowStyle Hidden -File `"$PSCommandPath`""
    $trigger = New-ScheduledTaskTrigger -Daily -At 05:30
    Register-ScheduledTask -TaskName "ob2h-sync" -Action $action -Trigger $trigger `
        -Description "OB2H sync push/pull PC<->VPS" -Force
    Write-Host "Задача ob2h-sync зарегистрирована (ежедневно 05:30)."
    exit 0
}

& $Binary sync push --peer vps
& $Binary sync pull --peer vps
& $Binary sync status
