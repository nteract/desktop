Unicode true

!include LogicLib.nsh
!include StrFunc.nsh
${StrLoc}

!define PRODUCTNAME "nteract Nightly"
!define MAINBINARYNAME "notebook"

!include "bootstrap.nsh"

Name "nteract NSIS bootstrap validation"
OutFile "nsis-bootstrap-validation.exe"
RequestExecutionLevel user

Var PassiveMode
Var UpdateMode

Section "ValidateInstallHooks"
  SetOutPath "$TEMP\nteract-bootstrap-validation"
  StrCpy $INSTDIR "$TEMP\nteract-bootstrap-validation"
  StrCpy $PassiveMode 1
  StrCpy $UpdateMode 0
  !insertmacro NSIS_HOOK_PREINSTALL
  !insertmacro NSIS_HOOK_POSTINSTALL
SectionEnd

Section "ValidateUninstallHooks"
  StrCpy $INSTDIR "$TEMP\nteract-bootstrap-validation"
  StrCpy $UpdateMode 0
  !insertmacro NSIS_HOOK_PREUNINSTALL
SectionEnd
