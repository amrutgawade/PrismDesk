; PrismDesk installer (NSIS + Modern UI 2).
; Per-user install (no admin / no UAC) into %LOCALAPPDATA%\Programs\PrismDesk.
; Packages the assembled portable folder from ..\dist\PrismDesk\.
; Build:  makensis packaging\installer.nsi   (see make-installer.ps1)

Unicode true
!include "MUI2.nsh"

!define APPNAME    "PrismDesk"
!define APPVER     "0.1.0"
!define PUBLISHER  "Amrut Gawade"
!define SRC        "..\dist\PrismDesk"
!define ICON       "..\crates\pd-engine\assets\icon\prismdesk.ico"
!define UNINSTKEY  "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"

Name "${APPNAME}"
OutFile "..\dist\${APPNAME}-${APPVER}-setup.exe"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\${APPNAME}"
InstallDirRegKey HKCU "Software\${APPNAME}" "InstallDir"
ShowInstDetails show
ShowUnInstDetails show

VIProductVersion "0.1.0.0"
VIAddVersionKey "ProductName" "${APPNAME}"
VIAddVersionKey "FileDescription" "${APPNAME} setup"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "(c) 2026 ${PUBLISHER}"
VIAddVersionKey "FileVersion" "${APPVER}.0"
VIAddVersionKey "ProductVersion" "${APPVER}.0"

!define MUI_ICON   "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APPNAME}.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${APPNAME}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetOutPath "$INSTDIR"
  File "${SRC}\PrismDesk.exe"
  File "${SRC}\scrcpy-server-v3.3.1.jar"
  File "${SRC}\prismdesk.ico"
  File "${SRC}\README.txt"

  SetOutPath "$INSTDIR\platform-tools"
  File "${SRC}\platform-tools\adb.exe"
  File "${SRC}\platform-tools\AdbWinApi.dll"
  File "${SRC}\platform-tools\AdbWinUsbApi.dll"

  ; Shortcuts (Start Menu + Desktop).
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\${APPNAME}.exe" "" "$INSTDIR\prismdesk.ico"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\Uninstall ${APPNAME}.lnk" "$INSTDIR\Uninstall.exe"
  CreateShortcut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${APPNAME}.exe" "" "$INSTDIR\prismdesk.ico"

  ; Uninstaller + registry.
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\${APPNAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayName" "${APPNAME}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayVersion" "${APPVER}"
  WriteRegStr HKCU "${UNINSTKEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayIcon" "$INSTDIR\prismdesk.ico"
  WriteRegStr HKCU "${UNINSTKEY}" "URLInfoAbout" "https://amrut.is-a.dev"
  WriteRegStr HKCU "${UNINSTKEY}" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
  WriteRegStr HKCU "${UNINSTKEY}" "QuietUninstallString" "$\"$INSTDIR\Uninstall.exe$\" /S"
  WriteRegStr HKCU "${UNINSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair" 1
  WriteRegDWORD HKCU "${UNINSTKEY}" "EstimatedSize" 14500
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\PrismDesk.exe"
  Delete "$INSTDIR\scrcpy-server-v3.3.1.jar"
  Delete "$INSTDIR\prismdesk.ico"
  Delete "$INSTDIR\README.txt"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir /r "$INSTDIR\platform-tools"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\Uninstall ${APPNAME}.lnk"
  RMDir "$SMPROGRAMS\${APPNAME}"
  Delete "$DESKTOP\${APPNAME}.lnk"

  DeleteRegKey HKCU "${UNINSTKEY}"
  DeleteRegKey HKCU "Software\${APPNAME}"
SectionEnd
