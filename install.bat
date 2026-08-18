@echo off
chcp 65001 > nul
echo Запуск установки OB2H в Hermes...
powershell -ExecutionPolicy Bypass -File "%~dp0scripts\install_hermes.ps1"
pause
