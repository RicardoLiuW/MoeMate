@echo off
chcp 65001 >nul
setlocal EnableExtensions
cd /d "%~dp0.."
title Deskpet asset pipeline

set "PYTHONUTF8=1"
set "PYTHONIOENCODING=utf-8"

echo ============================================================
echo  Deskpet asset pipeline
echo  Root : %CD%
echo  Log  : %~dp0run_all.log
echo.
echo  Finished clips are skipped automatically.
echo  Closing the window is safe - run this file again to resume.
echo ============================================================
echo.

set "PY=python"
python -c "import sys" >nul 2>nul && goto :have_python
set "PY=py -3"
py -3 -c "import sys" >nul 2>nul && goto :have_python

echo [ERROR] Python not found.
echo         Install Python 3.9 or newer from https://www.python.org/downloads/
echo         Tick "Add python.exe to PATH" during setup, then run this file again.
set "RC=1"
goto :finish

:have_python
echo Python: %PY%
echo.
%PY% -X utf8 "%~dp0run_all.py" %*
set "RC=%ERRORLEVEL%"
echo.

if "%RC%"=="0" goto :ok

echo ------------------------------------------------------------
echo [NOT FINISHED] exit code %RC%
echo   See the output above and %~dp0run_all.log
echo   Run this file again to continue; finished clips are not redone.
echo ------------------------------------------------------------
goto :finish

:ok
echo ------------------------------------------------------------
echo [DONE] summary  : "assets\QA_SUMMARY.md"
echo [DONE] per char : "assets\chars\<char>\QA_REPORT.md"
echo ------------------------------------------------------------

:finish
if defined DESKPET_NO_PAUSE exit /b %RC%
echo.
pause
exit /b %RC%
