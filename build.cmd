@echo off
REM The stale 32-bit dlltool in C:\MinGW breaks the windows-gnu target, so put
REM the WinLibs mingw64 toolchain ahead of it for the duration of the build.
setlocal
set "MINGW64=%LOCALAPPDATA%\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.MSVCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin"
if not exist "%MINGW64%\dlltool.exe" (
    echo [build] mingw64 not found in %MINGW64%
    echo [build] check the WinLibs install or fix PATH by hand
    exit /b 1
)
set "PATH=%MINGW64%;%PATH%"
cargo %*
