; Installer of the native Agu Chat (GPUI). Built by build-installer.ps1; see docs/GPUI-PORT.md "Installer and updates".
;
; Same identity as the old Electron app's NSIS installer: appId lol.agu.chat, productName
; "Agu Chat", per-user, default folder %LOCALAPPDATA%\Programs\agu-chat, "Agu Chat.exe", "Uninstall Agu Chat.exe",
; the same uninstall registry key, so installing this over an Electron install replaces it in place (shortcuts,
; Apps & features entry and the user's data in %APPDATA%\Agu Chat all carry over).
;
; Command line (all optional):  /S  silent    /D=dir  install folder (must be last)    /R  start the app afterwards
; (an interactive install always starts it)    /NOWAIT  do not wait for a running copy to exit.
; The in-app updater runs:  "Agu-Chat-Setup-<ver>-x64.exe" /S /R   and then quits the app, so the
; installer waits (up to 30 s) for "Agu Chat.exe" to release its file before it overwrites it.
Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user
ManifestDPIAware true

!ifndef VERSION
  !error "pass /DVERSION=<x.y.z[-pre]>"
!endif
!ifndef EXE_SRC
  !error "pass /DEXE_SRC=<path to agu-chat-gpui.exe>"
!endif
!ifndef ICON_SRC
  !error "pass /DICON_SRC=<path to app.ico>"
!endif
!ifndef OUT_FILE
  !define OUT_FILE "Agu-Chat-Setup-${VERSION}-x64.exe"
!endif

!define PRODUCT "Agu Chat"
!define PUBLISHER "Esmount"
!define EXE_NAME "Agu Chat.exe"
!define UNINSTALLER "Uninstall Agu Chat.exe"
!ifdef TEST_BUILD
  ; A test build must never touch the real install's registry key or shortcuts: own key, own shortcut name.
  !define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\agu-chat-installer-test"
  !define SHORTCUT "Agu Chat (installer test)"
  !define DEFAULT_DIR "$TEMP\agu-chat-installer-test"
!else
  !define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\d7530026-c2b3-572b-a28e-234d84faefc5"
  !define SHORTCUT "${PRODUCT}"
  !define DEFAULT_DIR "$LOCALAPPDATA\Programs\agu-chat"
!endif

Name "${PRODUCT}"
OutFile "${OUT_FILE}"
InstallDir "${DEFAULT_DIR}"
Icon "${ICON_SRC}"
UninstallIcon "${ICON_SRC}"
BrandingText " "
ShowInstDetails nevershow
ShowUnInstDetails nevershow
AutoCloseWindow true

VIProductVersion "0.0.0.0"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "LegalCopyright" "${PUBLISHER}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "FileDescription" "${PRODUCT} setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!include "LogicLib.nsh"
!include "FileFunc.nsh"

Page instfiles
UninstPage instfiles

Var Silent
Var RunAfter
Var NoWait

Function .onInit
  ${GetParameters} $R0
  StrCpy $Silent 0
  IfSilent 0 +2
  StrCpy $Silent 1
  StrCpy $RunAfter 0
  StrCpy $NoWait 0
  ClearErrors
  ${GetOptions} $R0 "/R" $R1
  ${IfNot} ${Errors}
    StrCpy $RunAfter 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/NOWAIT" $R1
  ${IfNot} ${Errors}
    StrCpy $NoWait 1
  ${EndIf}
  ClearErrors
FunctionEnd

; The running app keeps "Agu Chat.exe" open; wait until it can be written (it exits right after starting us).
Function WaitForExe
  ${If} $NoWait == 1
    Return
  ${EndIf}
  ${IfNot} ${FileExists} "$INSTDIR\${EXE_NAME}"
    Return
  ${EndIf}
  StrCpy $R0 0
  loop:
    ClearErrors
    FileOpen $R1 "$INSTDIR\${EXE_NAME}" a
    ${IfNot} ${Errors}
      FileClose $R1
      Return
    ${EndIf}
    IntOp $R0 $R0 + 1
    ${If} $R0 > 60
      ${If} $Silent == 1
        Abort
      ${EndIf}
      MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${PRODUCT} is still running. Close it, then try again." IDRETRY loop
      Abort
    ${EndIf}
    Sleep 500
    Goto loop
FunctionEnd

; An Electron install of Agu Chat leaves its runtime in the folder; those files are ours to replace. Only known
; Electron names are removed, never the folder as a whole.
Function CleanElectron
  ${IfNot} ${FileExists} "$INSTDIR\resources\app.asar"
    Return
  ${EndIf}
  RMDir /r "$INSTDIR\locales"
  RMDir /r "$INSTDIR\resources"
  Delete "$INSTDIR\*.pak"
  Delete "$INSTDIR\*.dll"
  Delete "$INSTDIR\*.dat"
  Delete "$INSTDIR\*.bin"
  Delete "$INSTDIR\LICENSE.electron.txt"
  Delete "$INSTDIR\LICENSES.chromium.html"
FunctionEnd

Section "Install"
  SetOutPath "$INSTDIR"
  Call WaitForExe
  Call CleanElectron
  File "/oname=${EXE_NAME}" "${EXE_SRC}"
  WriteUninstaller "$INSTDIR\${UNINSTALLER}"

  CreateShortCut "$SMPROGRAMS\${SHORTCUT}.lnk" "$INSTDIR\${EXE_NAME}" "" "$INSTDIR\${EXE_NAME}" 0
!ifndef TEST_BUILD
  CreateShortCut "$DESKTOP\${SHORTCUT}.lnk" "$INSTDIR\${EXE_NAME}" "" "$INSTDIR\${EXE_NAME}" 0
!endif

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT} ${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE_NAME},0"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\${UNINSTALLER}" /currentuser'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\${UNINSTALLER}" /currentuser /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $0

  ${If} $Silent == 0
  ${OrIf} $RunAfter == 1
    Exec '"$INSTDIR\${EXE_NAME}"'
  ${EndIf}
SectionEnd

Section "Uninstall"
  ; The user's data (%APPDATA%\Agu Chat) is never touched.
  Delete "$INSTDIR\${EXE_NAME}"
  Delete "$INSTDIR\${UNINSTALLER}"
  Delete "$SMPROGRAMS\${SHORTCUT}.lnk"
!ifndef TEST_BUILD
  Delete "$DESKTOP\${SHORTCUT}.lnk"
!endif
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Agu Chat"
  RMDir "$INSTDIR"
SectionEnd
