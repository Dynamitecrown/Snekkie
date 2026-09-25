; Snekkie installer (NSIS 3).
;
; Installs per user into %LOCALAPPDATA%\Programs\Snekkie, so it needs no
; administrator rights. Running a newer installer over an existing install
; upgrades it in place: same folder, same shortcuts, one entry in
; Apps & features. Saved sessions and settings live in %APPDATA%\snekkie
; and are never touched by installing, upgrading or uninstalling.
;
; Build (from the repository root):
;   makensis -DVERSION=2.0.0 -DEXE=target\release\snekkie.exe installer\snekkie.nsi
;
; Unattended use:
;   Snekkie-Setup-2.0.0.exe /S            install or upgrade silently
;   Snekkie-Setup-2.0.0.exe /S /D=C:\Dir  ...into a specific folder (first install)
;   "%LOCALAPPDATA%\Programs\Snekkie\uninstall.exe" /S

; A 64-bit installer where this NSIS has the stubs for one (Linux packages
; do; release builds are made there). The official Windows NSIS only ships
; 32-bit stubs, and that installer works just as well: everything it
; touches (HKCU, %LOCALAPPDATA%) is shared between 32- and 64-bit programs.
!if /FileExists "${NSISDIR}\Stubs\lzma_solid-amd64-unicode"
  Target amd64-unicode
!else
  Unicode true
!endif

!ifndef VERSION
  !error "Pass the version: makensis -DVERSION=x.y.z ..."
!endif
!ifndef EXE
  !define EXE "..\target\release\snekkie.exe"
!endif
!ifndef OUTFILE
  !define OUTFILE "..\dist\Snekkie-Setup-${VERSION}.exe"
!endif
; Windows version resources need four numbers; drop any "-beta.1" suffix.
!searchparse /noerrors "${VERSION}" "" VERSION_MAJOR "." VERSION_MINOR "." VERSION_PATCH "-"
!ifndef VERSION_PATCH
  !searchparse "${VERSION}" "" VERSION_MAJOR "." VERSION_MINOR "." VERSION_PATCH
!endif

!define APP_NAME "Snekkie"
!define PUBLISHER "Snekkie"
!define APP_KEY "Software\Snekkie"
; Never change this: it is how an upgrade finds the previous install.
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Snekkie"
; Held by snekkie.exe while it runs (see APP_MUTEX in src/lib.rs).
!define APP_MUTEX "Snekkie.Running"

Name "${APP_NAME}"
OutFile "${OUTFILE}"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\Snekkie"
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"
SetCompressor /SOLID lzma
ManifestDPIAware true
BrandingText "${APP_NAME} ${VERSION}"

VIProductVersion "${VERSION_MAJOR}.${VERSION_MINOR}.${VERSION_PATCH}.0"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APP_NAME} installer"
VIAddVersionKey "LegalCopyright" "GPL-3.0-or-later"
VIAddVersionKey "CompanyName" "${PUBLISHER}"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "WordFunc.nsh"
!include "FileFunc.nsh"

Var InstalledVersion
Var IsUpgrade
Var WelcomeText

!define MUI_ICON "..\assets\icon.ico"
!define MUI_UNICON "..\assets\icon.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP "welcome.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "welcome.bmp"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "${APP_NAME} ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "$WelcomeText"
!insertmacro MUI_PAGE_WELCOME
; The GPL is a licence to share and change the program, not terms you
; must accept to use it, so this page informs rather than asks. Upgrades
; skip it: they've seen it.
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipOnUpgrade
!define MUI_LICENSEPAGE_TEXT_TOP "Snekkie is free software, licensed under the GNU GPL v3 or later."
!define MUI_LICENSEPAGE_TEXT_BOTTOM "You don't need to accept this licence to use Snekkie. It sets out your rights to share and change it."
!define MUI_LICENSEPAGE_BUTTON "$(^NextBtn)"
!insertmacro MUI_PAGE_LICENSE "..\LICENSE"
; An upgrade goes where the existing install is, so don't offer a choice.
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipOnUpgrade
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\snekkie.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP_NAME}"
; The "show readme" box, repurposed as the usual desktop shortcut option.
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Create a desktop shortcut"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateDesktopShortcut
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; Wait until Snekkie isn't running, asking the user to close it. Aborts
; if they cancel, or straight away in silent mode, where there's nobody to
; ask and replacing a running exe would fail anyway.
!macro WAIT_FOR_APP_TO_CLOSE UN
Function ${UN}WaitForAppToClose
  check:
    ; SYNCHRONIZE access is enough to see whether the mutex exists.
    System::Call 'kernel32::OpenMutexW(i 0x00100000, i 0, w "${APP_MUTEX}") p .r1'
    ${If} $1 == 0
      Return
    ${EndIf}
    System::Call 'kernel32::CloseHandle(p r1)'
    ${If} ${Silent}
      SetErrorLevel 5
      Abort "${APP_NAME} is running. Close it and run this again."
    ${EndIf}
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION \
      "${APP_NAME} is running.$\n$\nClose it (open sessions will be disconnected), then click Retry." \
      /SD IDCANCEL IDRETRY check
    Abort
FunctionEnd
!macroend
!insertmacro WAIT_FOR_APP_TO_CLOSE ""
!insertmacro WAIT_FOR_APP_TO_CLOSE "un."

Function .onInit
  StrCpy $IsUpgrade 0
  ReadRegStr $InstalledVersion HKCU "${UNINSTALL_KEY}" "DisplayVersion"
  ${If} $InstalledVersion == ""
    StrCpy $WelcomeText "Setup will install ${APP_NAME} ${VERSION} on your computer.$\r$\n$\r$\nNo administrator rights are needed. Click Next to continue."
    Return
  ${EndIf}

  StrCpy $IsUpgrade 1
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "InstallLocation"
  ${If} $1 != ""
    StrCpy $INSTDIR $1
  ${EndIf}

  ${VersionCompare} "$InstalledVersion" "${VERSION}" $2
  ${If} $2 == 1
    ; What's installed is newer than this installer.
    MessageBox MB_YESNO|MB_ICONQUESTION \
      "${APP_NAME} $InstalledVersion is installed, which is newer than ${VERSION}.$\n$\nReplace it with the older version?" \
      /SD IDNO IDYES downgrade
    Abort
    downgrade:
  ${EndIf}
  ${If} $2 == 0
    StrCpy $WelcomeText "${APP_NAME} ${VERSION} is already installed.$\r$\n$\r$\nClick Next to reinstall it. Your saved sessions and settings are kept."
  ${Else}
    StrCpy $WelcomeText "Setup will update ${APP_NAME} $InstalledVersion to ${VERSION}.$\r$\n$\r$\nYour saved sessions and settings are kept. Click Next to continue."
  ${EndIf}
FunctionEnd

Function SkipOnUpgrade
  ${If} $IsUpgrade == 1
    Abort
  ${EndIf}
FunctionEnd

Function CreateDesktopShortcut
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\snekkie.exe"
FunctionEnd

Section "Install"
  Call WaitForAppToClose

  SetOutPath "$INSTDIR"
  SetOverwrite on
  File "/oname=snekkie.exe" "${EXE}"
  File "/oname=LICENSE.txt" "..\LICENSE"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP_NAME}.lnk" "$INSTDIR\snekkie.exe"
  ; Keep an existing desktop shortcut pointing at the new exe.
  ${If} ${FileExists} "$DESKTOP\${APP_NAME}.lnk"
    CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\snekkie.exe"
  ${EndIf}

  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${APP_KEY}" "Version" "${VERSION}"

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\snekkie.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/Dynamitecrown/Snekkie"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" "$0"
SectionEnd

Function un.onInit
  ; Nothing extra; confirmation happens on the uninstall page.
FunctionEnd

Section "Uninstall"
  Call un.WaitForAppToClose

  Delete "$INSTDIR\snekkie.exe"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APP_NAME}.lnk"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "${APP_KEY}"
  ; %APPDATA%\snekkie (saved sessions, settings) is left in place on
  ; purpose, so reinstalling later picks up where you left off.
SectionEnd
