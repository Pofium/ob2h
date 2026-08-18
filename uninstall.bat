@echo off
chcp 65001 > nul
echo Запуск удаления OB2H из Hermes...
powershell -ExecutionPolicy Bypass -File "%~dp0scripts\uninstall_hermes.ps1"
pause
