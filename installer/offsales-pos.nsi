; Off Sales POS installer (NSIS 3, Modern UI 2).
;
; Installs per user into %LOCALAPPDATA%\Programs\Off Sales POS with no admin
; prompt. That folder is writable by the cashier's account, which the in-app
; updater needs in order to replace the exe. Sales data lives separately in
; %APPDATA%\OffSales\POS and is never touched by install, upgrade or uninstall.
;
; Build with installer\build.ps1, or directly:
;   makensis /DVERSION=0.1.0 /DEXE=..\target\release\offsales-pos.exe /DOUTDIR=..\dist installer\offsales-pos.nsi

Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !error "Pass /DVERSION=x.y.z"
!endif
!ifndef EXE
  !define EXE "..\target\release\offsales-pos.exe"
!endif
!ifndef OUTDIR
  !define OUTDIR "..\dist"
!endif

!define APP_NAME   "Off Sales POS"
!define EXE_NAME   "offsales-pos.exe"
!define PUBLISHER  "Off Sales"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\OffSalesPOS"

Name "${APP_NAME}"
OutFile "${OUTDIR}\OffSalesPOS-Setup-${VERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
InstallDirRegKey HKCU "${UNINST_KEY}" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "${APP_NAME} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APP_NAME} installer"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "${PUBLISHER}"

; ---------------------------------------------------------------- pages
ManifestDPIAware true
!define MUI_ICON   "..\assets\icon.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP "..\assets\installer-side.bmp"
!define MUI_WELCOMEFINISHPAGE_BITMAP_STRETCH FitControl
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "..\assets\installer-side.bmp"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_RIGHT
!define MUI_HEADERIMAGE_BITMAP "..\assets\installer-header.bmp"
!define MUI_HEADERIMAGE_BITMAP_STRETCH FitControl
!define MUI_HEADERIMAGE_UNBITMAP "..\assets\installer-header.bmp"
!define MUI_UNICON "..\assets\icon.ico"
!define MUI_ABORTWARNING

!define MUI_WELCOMEPAGE_TEXT "This installs ${APP_NAME} ${VERSION} on this computer.$\r$\n$\r$\nIf the till is already installed, it will be upgraded. Your products, sales and backups are kept.$\r$\n$\r$\nClick Next to continue."
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE_NAME}"
!define MUI_FINISHPAGE_RUN_TEXT "Open ${APP_NAME} now"
; The "readme" checkbox is used to offer a desktop shortcut.
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Add a shortcut to the desktop"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateDesktopShortcut
!insertmacro MUI_PAGE_FINISH

!define MUI_UNCONFIRMPAGE_TEXT_TOP "${APP_NAME} will be removed from this computer. Your products, sales and backups are kept, so reinstalling brings everything back."
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; ---------------------------------------------------------------- helpers

; The exe can't be replaced while the till is open, so ask the cashier to close it.
; Windows refuses write access to a running exe, which is how "open" is detected.
!macro WaitForAppToClose
  ${Do}
    ${IfNot} ${FileExists} "$INSTDIR\${EXE_NAME}"
      ${Break}
    ${EndIf}
    ClearErrors
    FileOpen $0 "$INSTDIR\${EXE_NAME}" a
    ${IfNot} ${Errors}
      FileClose $0
      ${Break}
    ${EndIf}
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${APP_NAME} is open. Finish any sale in progress, close the till, then click Retry." /SD IDCANCEL IDRETRY +2
    Abort
  ${Loop}
!macroend

Function CreateDesktopShortcut
  CreateShortCut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${EXE_NAME}" "" "$INSTDIR\icon.ico" 0
FunctionEnd

; ---------------------------------------------------------------- install
Section "Install"
  !insertmacro WaitForAppToClose

  SetOutPath "$INSTDIR"
  File "/oname=${EXE_NAME}" "${EXE}"
  File "..\assets\icon.ico"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortCut "$SMPROGRAMS\${APP_NAME}.lnk" "$INSTDIR\${EXE_NAME}" "" "$INSTDIR\icon.ico" 0
  ; Refresh an existing desktop shortcut on upgrade, but don't add one unasked.
  ${If} ${FileExists} "$DESKTOP\${APP_NAME}.lnk"
    Call CreateDesktopShortcut
  ${EndIf}

  ; Apps & features entry
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayName"          "${APP_NAME}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayVersion"       "${VERSION}"
  WriteRegStr   HKCU "${UNINST_KEY}" "Publisher"            "${PUBLISHER}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayIcon"          "$INSTDIR\icon.ico"
  WriteRegStr   HKCU "${UNINST_KEY}" "InstallLocation"      "$INSTDIR"
  WriteRegStr   HKCU "${UNINST_KEY}" "UninstallString"      '"$INSTDIR\uninstall.exe"'
  WriteRegStr   HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  SectionGetSize 0 $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" $0
SectionEnd

; ---------------------------------------------------------------- uninstall
Section "Uninstall"
  !insertmacro WaitForAppToClose

  Delete "$INSTDIR\${EXE_NAME}"
  Delete "$INSTDIR\icon.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir  "$INSTDIR" ; only removed if empty

  Delete "$SMPROGRAMS\${APP_NAME}.lnk"
  Delete "$DESKTOP\${APP_NAME}.lnk"
  DeleteRegKey HKCU "${UNINST_KEY}"
  ; Data in %APPDATA%\OffSales\POS and Documents\OffSales POS Backups is intentionally kept.
SectionEnd
