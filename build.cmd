@echo off
REM Optional wrapper around cargo for the x86_64-pc-windows-gnu toolchain.
REM
REM An old 32-bit dlltool (e.g. from C:\MinGW) earlier in PATH breaks the gnu
REM target with "Invalid bfd target". This puts a 64-bit mingw toolchain ahead
REM of it for the duration of the build only. With the msvc toolchain none of
REM this is needed: run cargo directly.
REM
REM Set MINGW64 to your mingw64\bin to override the WinLibs (winget) default.
setlocal
if not defined MINGW64 set "MINGW64=%LOCALAPPDATA%\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.MSVCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin"
if not exist "%MINGW64%\dlltool.exe" (
    echo [build] mingw64 not found in %MINGW64%
    echo [build] set MINGW64 to your mingw64\bin, or build with the msvc toolchain
    exit /b 1
)
set "PATH=%MINGW64%;%PATH%"
cargo %*
