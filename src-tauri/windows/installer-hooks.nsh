; LSDJ's app-managed assets are intentionally outside the installer's ownership.
; Tauri's current-user NSIS default and LSDJ's shallow data root are both under
; $LOCALAPPDATA\LSDJ, so uninstall removes only declared application payloads.
; The data root is removed recursively only after this explicit, marker-guarded
; opt-in. /PURGE-LSDJ-DATA is the equivalent explicit choice for automation.

!define LSDJ_DATA_ROOT "$LOCALAPPDATA\LSDJ"
!define LSDJ_DATA_MARKER "${LSDJ_DATA_ROOT}\.lsdj-data-root"
Var LsdjDataRemovalFailed

; Return 1 in $R9 only when the marker contains LSDJ's exact application ID.
; Existence alone is insufficient authorization for recursive deletion.
Function un.LsdjDataMarkerIsValid
  StrCpy $R9 0
  ClearErrors
  FileOpen $R7 "${LSDJ_DATA_MARKER}" r
  IfErrors lsdj_marker_done
  FileRead $R7 $R8
  FileClose $R7
  StrCmp $R8 "works.protocol.lsdj" 0 lsdj_marker_done
  StrCpy $R9 1
  lsdj_marker_done:
FunctionEnd

!macro NSIS_HOOK_POSTINSTALL
  CreateDirectory "${LSDJ_DATA_ROOT}"
  FileOpen $R8 "${LSDJ_DATA_MARKER}" w
  FileWrite $R8 "works.protocol.lsdj"
  FileClose $R8
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; The normal checkbox is deliberately unchecked by default. Silent removal
  ; must name the destructive option; /S alone always preserves user assets.
  StrCpy $LsdjDataRemovalFailed 0
  ClearErrors
  ${GetOptions} $CMDLINE "/PURGE-LSDJ-DATA" $R8
  ${IfNot} ${Errors}
    StrCpy $DeleteAppDataCheckboxState 1
  ${EndIf}

  ${If} $DeleteAppDataCheckboxState = 1
    Call un.LsdjDataMarkerIsValid
    ${If} $R9 != 1
      StrCpy $LsdjDataRemovalFailed 1
      DetailPrint "Refusing to remove LSDJ data: ownership marker is missing or invalid at ${LSDJ_DATA_ROOT}"
      StrCpy $DeleteAppDataCheckboxState 0
      ${IfNot} ${Silent}
        MessageBox MB_ICONSTOP|MB_OK "LSDJ will preserve the data at:$\n${LSDJ_DATA_ROOT}$\n$\nThe ownership marker is missing or invalid, so automatic removal is unsafe."
      ${EndIf}
      Goto lsdj_data_decision_done
    ${EndIf}

    ; GetSize reports KiB. It is computed after the user ticks the checkbox and
    ; disclosed together with the exact target before any recursive removal.
    ${GetSize} "${LSDJ_DATA_ROOT}" "/S=0K" $R8 $R9 $R7
    DetailPrint "Selected LSDJ data removal: ${LSDJ_DATA_ROOT} ($R8 KiB)"
    ${IfNot} ${Silent}
      MessageBox MB_ICONEXCLAMATION|MB_YESNO|MB_DEFBUTTON2 "Permanently remove downloaded models, runtimes, settings, and user data?$\n$\nLocation: ${LSDJ_DATA_ROOT}$\nSize: $R8 KiB$\n$\nThis cannot be undone." IDYES lsdj_confirm_data_removal IDNO lsdj_keep_data
      lsdj_keep_data:
        StrCpy $DeleteAppDataCheckboxState 0
        Goto lsdj_data_decision_done
      lsdj_confirm_data_removal:
    ${EndIf}
  ${EndIf}
  lsdj_data_decision_done:
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; Re-check immediately before the destructive operation. Never broaden this
    ; target or replace it with a computed parent directory.
    Call un.LsdjDataMarkerIsValid
    ${If} $R9 = 1
      RMDir /r "${LSDJ_DATA_ROOT}"
    ${Else}
      DetailPrint "LSDJ data was preserved because its ownership marker disappeared or became invalid"
      StrCpy $LsdjDataRemovalFailed 1
    ${EndIf}
  ${EndIf}
  ${If} $LsdjDataRemovalFailed = 1
    SetErrorLevel 2
  ${EndIf}
!macroend
