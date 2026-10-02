; Snekkie installer (NSIS 3).
;
; Installs per user into %LOCALAPPDATA%\Programs\Snekkie, so it needs no
; administrator rights. Running a newer installer over an existing install
; upgrades it in place: same folder, same shortcuts, one entry in
; Apps & features. Saved sessions and settings live in %APPDATA%\snekkie
; are kept during upgrades. Network privacy choices are stored separately
; in %APPDATA%\snekkie\network.ini and can be changed in Preferences.
;
; Build (from the repository root):
;   makensis -DVERSION=2.5.0 -DEXE=target\release\snekkie.exe installer\snekkie.nsi
;
; Unattended use:
;   Snekkie-Setup-2.5.0.exe /S            install or upgrade silently
;   Snekkie-Setup-2.5.0.exe /S /D=C:\Dir  ...into a specific folder (first install)
;   "%LOCALAPPDATA%\Programs\Snekkie\uninstall.exe" /S
;
; Snekkie's own "Update now" runs the new installer with /UPDATE and then
; closes. On an existing install that skips straight to installing: it
; waits for Snekkie to close, upgrades it, and starts it again.

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
!include "nsDialogs.nsh"

Var InstalledVersion
Var IsUpgrade
Var IsUpdate
Var WelcomeText
Var OfflineMode
Var CheckUpdates
Var NetworkChoiceChanged
Var NetworkPage
Var OfflineCheckbox
Var UpdatesCheckbox

!define MUI_ICON "..\assets\icon.ico"
!define MUI_UNICON "..\assets\icon.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP "welcome.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "welcome.bmp"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "${APP_NAME} ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "$WelcomeText"
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipOnUpdate
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
Page custom NetworkOptionsCreate NetworkOptionsLeave
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\snekkie.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP_NAME}"
; The "show readme" box, repurposed as the usual desktop shortcut option.
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Create a desktop shortcut"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateDesktopShortcut
; An update restarts Snekkie by itself (see .onInstSuccess).
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipOnUpdate
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; Wait until Snekkie isn't running, asking the user to close it. Aborts
; if they cancel, or straight away in silent mode, where there's nobody to
; ask and replacing a running exe would fail anyway. An update started from
; Snekkie first gives it up to 30 seconds to close by itself.
!macro WAIT_FOR_APP_TO_CLOSE UN
Function ${UN}WaitForAppToClose
  StrCpy $R9 0
  check:
    ; SYNCHRONIZE access is enough to see whether the mutex exists.
    System::Call 'kernel32::OpenMutexW(i 0x00100000, i 0, w "${APP_MUTEX}") p .r1'
    ${If} $1 == 0
      ${If} $R9 > 0
        ; Windows can hold on to the exe for a moment after the process
        ; has let go of the mutex.
        Sleep 1000
      ${EndIf}
      Return
    ${EndIf}
    System::Call 'kernel32::CloseHandle(p r1)'
    ${If} $IsUpdate == 1
    ${AndIf} $R9 < 120
      ${If} $R9 == 0
        DetailPrint "Waiting for ${APP_NAME} to close..."
      ${EndIf}
      IntOp $R9 $R9 + 1
      Sleep 250
      Goto check
    ${EndIf}
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
  StrCpy $IsUpdate 0
  StrCpy $OfflineMode 0
  StrCpy $CheckUpdates 1
  StrCpy $NetworkChoiceChanged 0
  ${If} ${FileExists} "$APPDATA\snekkie\network.ini"
    ReadINIStr $OfflineMode "$APPDATA\snekkie\network.ini" "Network" "OfflineMode"
    ReadINIStr $CheckUpdates "$APPDATA\snekkie\network.ini" "Network" "CheckForUpdates"
    ; A damaged policy fails closed, as it does in the application.
    ${If} $OfflineMode != 0
      StrCpy $OfflineMode 1
      StrCpy $CheckUpdates 0
    ${EndIf}
    ${If} $CheckUpdates != 1
      StrCpy $CheckUpdates 0
    ${EndIf}
  ${EndIf}
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/OFFLINE" $1
  ${IfNot} ${Errors}
    StrCpy $OfflineMode 1
    StrCpy $CheckUpdates 0
    StrCpy $NetworkChoiceChanged 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $0 "/ONLINE" $1
  ${IfNot} ${Errors}
    StrCpy $OfflineMode 0
    StrCpy $NetworkChoiceChanged 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $0 "/NOUPDATES" $1
  ${IfNot} ${Errors}
    StrCpy $CheckUpdates 0
    StrCpy $NetworkChoiceChanged 1
  ${EndIf}
  ReadRegStr $InstalledVersion HKCU "${UNINSTALL_KEY}" "DisplayVersion"
  ${If} $InstalledVersion == ""
    StrCpy $NetworkChoiceChanged 1
    StrCpy $WelcomeText "Setup will install ${APP_NAME} ${VERSION} on your computer.$\r$\n$\r$\nNo administrator rights are needed. Click Next to continue."
    Return
  ${EndIf}

  StrCpy $IsUpgrade 1
  ; Only an existing install can be updated this way; a first install
  ; always gets the full set of pages.
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/UPDATE" $1
  ${IfNot} ${Errors}
    StrCpy $IsUpdate 1
  ${EndIf}
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

Function NetworkOptionsCreate
  ${If} $IsUpdate == 1
    Abort
  ${EndIf}
  !insertmacro MUI_HEADER_TEXT "Network privacy" "Choose how Snekkie uses online services."
  nsDialogs::Create 1018
  Pop $NetworkPage
  ${If} $NetworkPage == error
    Abort
  ${EndIf}
  ${NSD_CreateCheckbox} 0 4u 100% 16u "Enable offline mode"
  Pop $OfflineCheckbox
  ${NSD_SetState} $OfflineCheckbox $OfflineMode
  ${NSD_OnClick} $OfflineCheckbox NetworkOfflineChanged
  ${NSD_CreateLabel} 10u 25u 95% 48u "Disable online update checks and downloads. Ask before every public IP connection and before resolving a hostname. Serial, localhost and private IP connections stay available."
  Pop $0
  ${NSD_CreateCheckbox} 0 80u 100% 16u "Check for updates when Snekkie starts"
  Pop $UpdatesCheckbox
  ${NSD_SetState} $UpdatesCheckbox $CheckUpdates
  ${NSD_OnClick} $UpdatesCheckbox NetworkUpdatesChanged
  ${If} $OfflineMode == 1
    EnableWindow $UpdatesCheckbox 0
  ${EndIf}
  ${NSD_CreateLabel} 0 108u 100% 38u "You can change these choices later in Settings > Preferences > General. Existing sessions, themes and passwords are not changed. In-app upgrades keep your privacy choices."
  Pop $0
  nsDialogs::Show
FunctionEnd

Function NetworkOfflineChanged
  Pop $0
  StrCpy $NetworkChoiceChanged 1
  ${NSD_GetState} $OfflineCheckbox $OfflineMode
  ${If} $OfflineMode == 1
    StrCpy $CheckUpdates 0
    ${NSD_Uncheck} $UpdatesCheckbox
    EnableWindow $UpdatesCheckbox 0
  ${Else}
    EnableWindow $UpdatesCheckbox 1
  ${EndIf}
FunctionEnd

Function NetworkUpdatesChanged
  Pop $0
  StrCpy $NetworkChoiceChanged 1
  ${NSD_GetState} $UpdatesCheckbox $CheckUpdates
FunctionEnd

Function NetworkOptionsLeave
  ${NSD_GetState} $OfflineCheckbox $OfflineMode
  ${NSD_GetState} $UpdatesCheckbox $CheckUpdates
FunctionEnd

Function SkipOnUpgrade
  ${If} $IsUpgrade == 1
    Abort
  ${EndIf}
FunctionEnd

Function SkipOnUpdate
  ${If} $IsUpdate == 1
    Abort
  ${EndIf}
FunctionEnd

Function .onInstSuccess
  ${If} $IsUpdate == 1
    Exec '"$INSTDIR\snekkie.exe"'
  ${EndIf}
FunctionEnd

Function CreateDesktopShortcut
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\snekkie.exe"
FunctionEnd

Section "Install"
  ${If} $IsUpdate == 1
    ; Nothing to read on the way out: carry on once installed.
    SetAutoClose true
  ${EndIf}
  Call WaitForAppToClose

  ${If} $NetworkChoiceChanged == 1
    ${If} $OfflineMode == 1
      StrCpy $CheckUpdates 0
    ${EndIf}
    CreateDirectory "$APPDATA\snekkie"
    ClearErrors
    WriteINIStr "$APPDATA\snekkie\network.ini" "Network" "OfflineMode" "$OfflineMode"
    WriteINIStr "$APPDATA\snekkie\network.ini" "Network" "CheckForUpdates" "$CheckUpdates"
    ${If} ${Errors}
      SetErrorLevel 6
      MessageBox MB_OK|MB_ICONSTOP "Could not save network privacy choices. Setup cannot continue safely." /SD IDOK
      Abort
    ${EndIf}
  ${EndIf}
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
