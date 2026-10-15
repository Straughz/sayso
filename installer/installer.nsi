; NVG Voice (Rust app) NSIS 3 installer. Per-user, no admin.
; Build: pwsh app/installer/build.ps1   (or makensis /DVERSION=x.y.z installer.nsi)
; Keeps the identity of the retired Python installer (same folder, uninstall
; key and Run value name) so an old install is upgraded in place.
;
; Silent: NVG-Voice-Setup-<ver>.exe /S [/AUTOSTART=1] [/NORUN=1] [/D=<dir>]

!define APP_NAME      "NVG Voice"
!define APP_EXE       "nvg-voice.exe"
!define OLD_EXE       "NVGVoice.exe"
!define PUBLISHER     "Nexus Vantage Group"
!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\NVG Voice"
!define RUN_KEY       "Software\Microsoft\Windows\CurrentVersion\Run"
!define WV2_GUID      "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!define WV2_URL       "https://go.microsoft.com/fwlink/p/?LinkId=2124703"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

Name "${APP_NAME} ${VERSION}"
OutFile "${__FILEDIR__}\out\NVG-Voice-Setup-${VERSION}.exe"
InstallDir "$LOCALAPPDATA\${APP_NAME}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Unicode true

!define MUI_ICON   "${__FILEDIR__}\..\src-tauri\icons\icon.ico"
!define MUI_UNICON "${__FILEDIR__}\..\src-tauri\icons\icon.ico"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${APP_NAME}"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchApp
!define MUI_FINISHPAGE_SHOWREADME
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Start ${APP_NAME} when I sign in"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION SetAutostart
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED_DUMMY ""
!undef MUI_FINISHPAGE_SHOWREADME_NOTCHECKED_DUMMY
!insertmacro MUI_PAGE_FINISH

!define MUI_UNCONFIRMPAGE_TEXT_TOP "This removes ${APP_NAME} from this PC. Your settings, API key, history and logs are kept."
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Function LaunchApp
  Exec '"$INSTDIR\${APP_EXE}"'
FunctionEnd

Function SetAutostart
  WriteRegStr HKCU "${RUN_KEY}" "NVGVoice" '"$INSTDIR\${APP_EXE}"'
FunctionEnd

; Waits (bounded) for ${APP_EXE} to be writable, or absent, so a File
; overwrite after taskkill does not abort on a slow process teardown.
; Up to 10 tries, 300 ms apart. Plain NSIS 3, no plugins.
!macro WaitForExeWritableBody suffix
  StrCpy $9 0
  wait_exe_loop_${suffix}:
    ${IfNot} ${FileExists} "$INSTDIR\${APP_EXE}"
      Return
    ${EndIf}
    ClearErrors
    FileOpen $8 "$INSTDIR\${APP_EXE}" a
    IfErrors wait_exe_locked_${suffix}
    FileClose $8
    Return
  wait_exe_locked_${suffix}:
    Sleep 300
    IntOp $9 $9 + 1
    ${If} $9 < 10
      Goto wait_exe_loop_${suffix}
    ${EndIf}
!macroend

Function WaitForExeWritable
  !insertmacro WaitForExeWritableBody inst
FunctionEnd

Function un.WaitForExeWritable
  !insertmacro WaitForExeWritableBody uninst
FunctionEnd

; Sets $0 to "1" if a WebView2 runtime version is registered.
Function CheckWebView2
  StrCpy $0 "0"
  ReadRegStr $1 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WV2_GUID}" "pv"
  ${If} $1 != ""
  ${AndIf} $1 != "0.0.0.0"
    StrCpy $0 "1"
    Return
  ${EndIf}
  ReadRegStr $1 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WV2_GUID}" "pv"
  ${If} $1 != ""
  ${AndIf} $1 != "0.0.0.0"
    StrCpy $0 "1"
  ${EndIf}
FunctionEnd

Section "Main Application" SecMain
  ; Close a running app (new Rust exe or the old Python exe).
  nsExec::Exec 'taskkill /F /IM ${APP_EXE}'
  Pop $0
  nsExec::Exec 'taskkill /F /IM ${OLD_EXE}'
  Pop $0
  Call WaitForExeWritable

  ; Upgrade from the Python install: bundled config copy and old exe.
  ; User data lives in %APPDATA% / %LOCALAPPDATA%\NVGVoice, not here.
  ${If} ${FileExists} "$INSTDIR\${OLD_EXE}"
    Delete "$INSTDIR\${OLD_EXE}"
    Delete "$DESKTOP\${APP_NAME}.lnk"
  ${EndIf}
  RMDir /r "$INSTDIR\config"

  ; WebView2 check (do not abort; dictation works without it).
  Call CheckWebView2
  ${If} $0 != "1"
    DetailPrint "${APP_NAME} needs the Microsoft Edge WebView2 Runtime for the tray panel and Settings. Download it from ${WV2_URL}"
    ${IfNot} ${Silent}
      MessageBox MB_OK|MB_ICONINFORMATION "${APP_NAME} needs the Microsoft Edge WebView2 Runtime for the tray panel and Settings. Dictation works without it.$\r$\n$\r$\nDownload it from:$\r$\n${WV2_URL}"
    ${EndIf}
  ${EndIf}

  SetOutPath "$INSTDIR"
  File "${__FILEDIR__}\..\target\release\${APP_EXE}"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  ; Shortcuts (same names as the Python install, so they are overwritten).
  Delete "$SMPROGRAMS\${APP_NAME}.lnk"
  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk" "$INSTDIR\Uninstall.exe"

  ; Uninstall registration.
  SectionGetSize ${SecMain} $2
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "DisplayName"          "${APP_NAME}"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "DisplayVersion"       "${VERSION}"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "Publisher"            "${PUBLISHER}"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "InstallLocation"      "$INSTDIR"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "UninstallString"      '"$INSTDIR\Uninstall.exe"'
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "DisplayIcon"          "$INSTDIR\${APP_EXE}"
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify"             1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair"             1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize"        $2

  ; Silent mode: no finish page, so apply the switches here.
  ${If} ${Silent}
    ${GetParameters} $R0
    ${GetOptions} $R0 "/AUTOSTART=" $R1
    ${If} $R1 == "1"
      Call SetAutostart
    ${EndIf}
    ${GetOptions} $R0 "/NORUN=" $R2
    ${If} $R2 != "1"
      Call LaunchApp
    ${EndIf}
  ${EndIf}
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /F /IM ${APP_EXE}'
  Pop $0
  nsExec::Exec 'taskkill /F /IM ${OLD_EXE}'
  Pop $0
  Call un.WaitForExeWritable

  DeleteRegValue HKCU "${RUN_KEY}" "NVGVoice"
  DeleteRegValue HKCU "${RUN_KEY}" "Vox"

  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir  "$INSTDIR"

  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk"
  RMDir  "$SMPROGRAMS\${APP_NAME}"

  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  ; %APPDATA%\NVGVoice and %LOCALAPPDATA%\NVGVoice (user data) are left alone.
SectionEnd
