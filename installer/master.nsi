; Stable downloader: version this independently of the Snekkie application.
; Build with scripts/build-master-installer.ps1 or the Master installer workflow.
Unicode true
!ifndef VERSION
  !error "Pass the master version from installer/master-version.txt"
!endif
!ifndef OUTFILE
  !define OUTFILE "..\dist\Snekkie-Setup.exe"
!endif
!searchparse "${VERSION}" "" VERSION_MAJOR "." VERSION_MINOR "." VERSION_PATCH

Name "Snekkie"
OutFile "${OUTFILE}"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\Snekkie"
InstallDirRegKey HKCU "Software\Snekkie" "InstallDir"
SetCompressor /SOLID lzma
ManifestDPIAware true
BrandingText "Snekkie"
VIProductVersion "${VERSION_MAJOR}.${VERSION_MINOR}.${VERSION_PATCH}.0"
VIAddVersionKey "ProductName" "Snekkie"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "Snekkie online installer"
VIAddVersionKey "LegalCopyright" "GPL-3.0-or-later"
VIAddVersionKey "CompanyName" "Snekkie"

!include "MUI2.nsh"
!include "LogicLib.nsh"
Var PowerShell
Var QuietArgument
Var SetupResult

!define MUI_ICON "..\assets\icon.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP "welcome.bmp"
!define MUI_WELCOMEPAGE_TITLE "Install the latest Snekkie"
!define MUI_WELCOMEPAGE_TEXT "Setup will download the latest stable Snekkie release from GitHub, verify the download, and open its installer.$\r$\n$\r$\nYou can then choose installation and network privacy settings. No administrator rights are needed.$\r$\n$\r$\nAn internet connection is required. For an offline installation, download a full installer from the release page.$\r$\n$\r$\nClick Install to continue."
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_TITLE "Snekkie setup finished"
!define MUI_FINISHPAGE_TEXT "The latest Snekkie installer has finished.$\r$\n$\r$\nClick Finish to close this window."
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  StrCpy $PowerShell "$SYSDIR\WindowsPowerShell\v1.0\powershell.exe"
  ; Use native 64-bit Windows PowerShell when this 32-bit installer runs on x64.
  ${If} ${FileExists} "$WINDIR\Sysnative\WindowsPowerShell\v1.0\powershell.exe"
    StrCpy $PowerShell "$WINDIR\Sysnative\WindowsPowerShell\v1.0\powershell.exe"
  ${EndIf}
  ${IfNot} ${FileExists} "$PowerShell"
    SetErrorLevel 2
    MessageBox MB_OK|MB_ICONSTOP "Windows PowerShell is unavailable. Download a full Snekkie installer from https://github.com/Dynamitecrown/Snekkie/releases/latest" /SD IDOK
    Abort
  ${EndIf}
  StrCpy $QuietArgument ""
  IfSilent 0 +2
    StrCpy $QuietArgument "-Silent"
FunctionEnd

Section "Download and install"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "master-installer.ps1"
  DetailPrint "Connecting to GitHub..."
  ; Pass paths through the environment, preserving spaces and trailing slashes.
  System::Call 'kernel32::SetEnvironmentVariableW(w "SNEKKIE_MASTER_DOWNLOAD_DIRECTORY", w "$PLUGINSDIR") i .r0'
  System::Call 'kernel32::SetEnvironmentVariableW(w "SNEKKIE_MASTER_INSTALL_DIRECTORY", w "$INSTDIR") i .r0'
!ifdef REQUIRE_SIGNED_RELEASE
  !define SIGNATURE_ARGUMENT "-RequireSignature"
!else
  !define SIGNATURE_ARGUMENT ""
!endif
  nsExec::ExecToLog '"$PowerShell" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\master-installer.ps1" $QuietArgument ${SIGNATURE_ARGUMENT}'
  Pop $SetupResult
  System::Call 'kernel32::SetEnvironmentVariableW(w "SNEKKIE_MASTER_DOWNLOAD_DIRECTORY", p 0)'
  System::Call 'kernel32::SetEnvironmentVariableW(w "SNEKKIE_MASTER_INSTALL_DIRECTORY", p 0)'
  ${If} $SetupResult != 0
    DetailPrint "Snekkie setup exited with status $SetupResult."
    ${If} $SetupResult == 1
      SetErrorLevel 1
      DetailPrint "Snekkie setup was cancelled."
    ${Else}
      SetErrorLevel 2
      MessageBox MB_OK|MB_ICONSTOP "Snekkie setup could not finish. See the details above, try again, or download a full installer from https://github.com/Dynamitecrown/Snekkie/releases/latest" /SD IDOK
    ${EndIf}
    Abort
  ${EndIf}
  SetErrorLevel 0
SectionEnd
