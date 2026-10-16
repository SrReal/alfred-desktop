; Public installation must never write into the personal application's folder.
!include "LogicLib.nsh"
!macro NSIS_HOOK_PREINSTALL
  ${If} ${FileExists} "$INSTDIR\AlfredPet.exe"
    SetErrorLevel 2
    MessageBox MB_OK|MB_ICONSTOP "Esta carpeta contiene la edicion personal de AlfredPet. Elija otra carpeta para AlfredPet Community." /SD IDOK
    Abort
  ${EndIf}
!macroend
