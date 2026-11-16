@echo off
rem Builds EagleView in release mode and packages it as
rem dist\EagleView-v<version>-windows-x64.zip
rem
rem Usage:  release.bat 0.0.3
rem         release.bat v0.0.3 --force    (replace an existing release of that version)

setlocal EnableExtensions
cd /d "%~dp0"

set "VERSION=%~1"
if "%VERSION%"=="" (
    echo Usage: release.bat ^<version^> [--force]   e.g. release.bat 0.0.3
    exit /b 1
)
if /i "%VERSION:~0,1%"=="v" set "VERSION=%VERSION:~1%"

set "FORCE="
if /i "%~2"=="--force" set "FORCE=1"
if /i "%~2"=="-f" set "FORCE=1"

echo %VERSION%| findstr /r /x "[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]* [0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*[-+][0-9A-Za-z.-]*" >nul
if errorlevel 1 (
    echo Error: version "%VERSION%" is not of the form MAJOR.MINOR.PATCH ^(e.g. 0.0.3^).
    exit /b 1
)

set "NAME=EagleView-v%VERSION%-windows-x64"
set "STAGE=dist\%NAME%"
set "ZIP=dist\%NAME%.zip"

set "EXISTS="
if exist "%STAGE%" set "EXISTS=1"
if exist "%ZIP%" set "EXISTS=1"
if defined EXISTS if not defined FORCE goto :exists
if exist "%STAGE%" rmdir /s /q "%STAGE%"
if exist "%ZIP%" del /q "%ZIP%"

echo Building release...
cargo build --release
if errorlevel 1 (
    echo Error: cargo build failed.
    exit /b 1
)

echo Staging %STAGE%...
mkdir "%STAGE%" || exit /b 1
copy /y "target\release\eagleview.exe" "%STAGE%\" >nul || exit /b 1
copy /y "sysml\tests\data\drone.sysml" "%STAGE%\example.sysml" >nul || exit /b 1
xcopy /e /i /q /y "themes" "%STAGE%\themes" >nul || exit /b 1

rem Ship the committed config, not the working copy: the app writes things
rem such as the last opened view back into eagleview.toml while it runs.
git show HEAD:eagleview.toml > "%STAGE%\eagleview.toml" 2>nul
if errorlevel 1 (
    echo Warning: could not read eagleview.toml from git; using the working copy.
    copy /y "eagleview.toml" "%STAGE%\eagleview.toml" >nul || exit /b 1
)

echo Zipping...
tar -a -c -f "%ZIP%" -C dist "%NAME%"
if errorlevel 1 (
    echo Error: creating the zip failed.
    exit /b 1
)

echo Created %ZIP%
exit /b 0

:exists
echo Error: %STAGE% already exists. Pass --force to replace it.
exit /b 1
