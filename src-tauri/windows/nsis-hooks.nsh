; NSIS pre-install hook for Waylode.
; Cleans up a prior GitPulse NSIS (-setup.exe) installation before installing
; Waylode so Add/Remove Programs does not retain a duplicate "GitPulse" entry.
; User profile data (%APPDATA%\GitPulse) and WebView2 data (%LOCALAPPDATA%\dev.vbcr.gitpulse)
; are preserved for Waylode's first-run migration.

!macro NSIS_HOOK_PREINSTALL
  Push $R0
  Push $R1
  Push $R2

  ; Check current-user uninstall entry written by GitPulse's NSIS installer.
  ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\GitPulse" "UninstallString"
  ${If} $R0 != ""
    ReadRegStr $R1 HKCU "Software\vbcr\GitPulse" ""
    ${If} $R1 != ""
    ${AndIf} ${FileExists} "$R1\uninstall.exe"
      ; Run the legacy uninstaller silently in-place (_?= keeps ExecWait synchronous;
      ; DeleteAppDataCheckboxState defaults to 0 so user data is preserved).
      ExecWait '"$R1\uninstall.exe" /S _?=$R1' $R2
      Delete "$R1\uninstall.exe"
      RMDir "$R1"
    ${EndIf}
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\GitPulse"
    DeleteRegKey HKCU "Software\vbcr\GitPulse"
    DeleteRegKey /ifempty HKCU "Software\vbcr"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "GitPulse"
  ${EndIf}

  Pop $R2
  Pop $R1
  Pop $R0
!macroend
