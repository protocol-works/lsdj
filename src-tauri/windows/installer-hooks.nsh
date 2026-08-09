; LSDJ's app-managed assets share Tauri's current-user install root at
; $LOCALAPPDATA\LSDJ.  Ownership must be established before Tauri copies any
; payload there.  Uninstall preserves the tree by default and recursively
; removes it only after explicit opt-in, exact-path checks, and reparse-safe
; validation. /PURGE-LSDJ-DATA is the equivalent explicit automation choice.

!define LSDJ_DATA_ROOT "$LOCALAPPDATA\LSDJ"
!define LSDJ_DATA_MARKER "${LSDJ_DATA_ROOT}\.lsdj-data-root"
!define LSDJ_DATA_MARKER_NEW "${LSDJ_DATA_ROOT}\.lsdj-data-root.new"
!define LSDJ_OWNER_ID "works.protocol.lsdj"
!define LSDJ_OWNER_ID_BYTES 19
!define LSDJ_FILE_ATTRIBUTE_DIRECTORY 0x10
!define LSDJ_FILE_ATTRIBUTE_REPARSE_POINT 0x400
!define LSDJ_FILE_ATTRIBUTE_NORMAL 0x80
!define LSDJ_FILE_FLAG_OPEN_REPARSE_POINT 0x200000
!define LSDJ_FILE_SHARE_READ 0x1
!define LSDJ_GENERIC_READ 0x80000000
!define LSDJ_GENERIC_WRITE 0x40000000
!define LSDJ_OPEN_EXISTING 3
!define LSDJ_CREATE_NEW 1
!define LSDJ_INVALID_FILE_ATTRIBUTES -1
!define LSDJ_INVALID_HANDLE_VALUE -1

Var LsdjCanonicalRootSafe
Var LsdjDeleteData
Var LsdjDeleteFailure
Var LsdjDataRemovalFailed
Var LsdjInstallRootState
Var LsdjMarkerSafe
Var LsdjOwnedRootSafe
Var LsdjRootEmpty
Var LsdjSafeLayout
Var LsdjTreeSafe

; GetFullPathNameW is lexical and does not traverse the candidate. The exact
; canonical target must equal canonical LOCALAPPDATA + \LSDJ; callers then
; separately reject a root reparse point before reading or changing it.
!macro LSDJ_DEFINE_CANONICAL_ROOT_VALIDATOR FUNCTION_NAME
Function ${FUNCTION_NAME}
  Push $R3
  Push $R4
  Push $R5
  Push $R6
  Push $R7
  Push $R8
  StrCpy $LsdjCanonicalRootSafe 0

  System::Call 'kernel32::GetFullPathNameW(w "$LOCALAPPDATA", i 1024, w .R7, p 0) i .R8'
  ${If} $R8 = 0
  ${OrIf} $R8 >= 1024
    Goto lsdj_canonical_done
  ${EndIf}
  StrCpy $R6 "$R7\LSDJ"
  System::Call 'kernel32::GetFullPathNameW(w R6, i 1024, w .R4, p 0) i .R3'
  ${If} $R3 = 0
  ${OrIf} $R3 >= 1024
    Goto lsdj_canonical_done
  ${EndIf}
  System::Call 'kernel32::GetFullPathNameW(w "${LSDJ_DATA_ROOT}", i 1024, w .R5, p 0) i .R8'
  ${If} $R8 = 0
  ${OrIf} $R8 >= 1024
    Goto lsdj_canonical_done
  ${EndIf}
  System::Call 'kernel32::lstrcmpiW(w R5, w R4) i .R8'
  ${If} $R8 = 0
    StrCpy $LsdjCanonicalRootSafe 1
  ${EndIf}

  lsdj_canonical_done:
  Pop $R8
  Pop $R7
  Pop $R6
  Pop $R5
  Pop $R4
  Pop $R3
FunctionEnd
!macroend

!insertmacro LSDJ_DEFINE_CANONICAL_ROOT_VALIDATOR LsdjCanonicalDataRootIsValid
!insertmacro LSDJ_DEFINE_CANONICAL_ROOT_VALIDATOR un.LsdjCanonicalDataRootIsValid

; Return LsdjMarkerSafe=1 only for a plain, non-reparse marker whose complete
; first line is the exact LSDJ application identifier. CreateFile opens the
; reparse entry itself and denies write/delete sharing. Keeping that native
; handle open while FileOpen reads the path prevents replacement between the
; attribute and content checks.
!macro LSDJ_DEFINE_MARKER_VALIDATOR FUNCTION_NAME
Function ${FUNCTION_NAME}
  Push $R5
  Push $R6
  Push $R7
  Push $R8
  StrCpy $LsdjMarkerSafe 0
  System::Call 'kernel32::CreateFileW(w "${LSDJ_DATA_MARKER}", i ${LSDJ_GENERIC_READ}, i ${LSDJ_FILE_SHARE_READ}, p 0, i ${LSDJ_OPEN_EXISTING}, i ${LSDJ_FILE_FLAG_OPEN_REPARSE_POINT}, p 0) p .R6'
  StrCmp $R6 ${LSDJ_INVALID_HANDLE_VALUE} lsdj_marker_done
  System::Call 'kernel32::GetFileInformationByHandle(p R6, *(&i4 .R7, &v48)) i .R8'
  ${If} $R8 = 0
    Goto lsdj_marker_close
  ${EndIf}
  IntOp $R8 $R7 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
  ${If} $R8 <> 0
    Goto lsdj_marker_close
  ${EndIf}
  IntOp $R8 $R7 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
  ${If} $R8 <> 0
    Goto lsdj_marker_close
  ${EndIf}
  ClearErrors
  FileOpen $R5 "${LSDJ_DATA_MARKER}" r
  IfErrors lsdj_marker_close
  FileRead $R5 $R8
  FileClose $R5
  StrCmp $R8 "${LSDJ_OWNER_ID}" 0 lsdj_marker_close
  StrCpy $LsdjMarkerSafe 1

  lsdj_marker_close:
  System::Call 'kernel32::CloseHandle(p R6)'
  lsdj_marker_done:
  Pop $R8
  Pop $R7
  Pop $R6
  Pop $R5
FunctionEnd
!macroend

!insertmacro LSDJ_DEFINE_MARKER_VALIDATOR LsdjDataMarkerIsValid
!insertmacro LSDJ_DEFINE_MARKER_VALIDATOR un.LsdjDataMarkerIsValid

; Create the marker without following or overwriting an entry raced into the
; temporary path. The fixed byte count is asserted by the packaging contracts.
Function LsdjCreateDataMarker
  Push $R6
  Push $R7
  Push $R8
  StrCpy $LsdjMarkerSafe 0
  StrCpy $R7 0
  System::Call 'kernel32::CreateFileW(w "${LSDJ_DATA_MARKER_NEW}", i ${LSDJ_GENERIC_WRITE}, i 0, p 0, i ${LSDJ_CREATE_NEW}, i ${LSDJ_FILE_ATTRIBUTE_NORMAL}|${LSDJ_FILE_FLAG_OPEN_REPARSE_POINT}, p 0) p .R6'
  StrCmp $R6 ${LSDJ_INVALID_HANDLE_VALUE} lsdj_marker_create_done
  System::Call 'kernel32::WriteFile(p R6, m "${LSDJ_OWNER_ID}", i ${LSDJ_OWNER_ID_BYTES}, *i .R8, p 0) i .R7'
  System::Call 'kernel32::CloseHandle(p R6)'
  ${If} $R7 = 0
  ${OrIf} $R8 != ${LSDJ_OWNER_ID_BYTES}
    Goto lsdj_marker_done
  ${EndIf}
  StrCpy $R7 0
  ClearErrors
  Rename "${LSDJ_DATA_MARKER_NEW}" "${LSDJ_DATA_MARKER}"
  IfErrors lsdj_marker_create_done
  StrCpy $R7 1
  Call LsdjDataMarkerIsValid
  ${If} $LsdjMarkerSafe = 1
    Goto lsdj_marker_create_done
  ${EndIf}
  StrCpy $LsdjMarkerSafe 0

  lsdj_marker_done:
  Delete "${LSDJ_DATA_MARKER_NEW}"
  lsdj_marker_create_done:
  ${If} $LsdjMarkerSafe != 1
    Delete "${LSDJ_DATA_MARKER_NEW}"
    ${If} $R7 = 1
      Delete "${LSDJ_DATA_MARKER}"
    ${EndIf}
  ${EndIf}
  Pop $R8
  Pop $R7
  Pop $R6
FunctionEnd

; A legacy app-created layout without an ownership marker is recognized only
; when its top level is exactly the five roots created by platform_paths.rs.
; Empty, partial, foreign, file-bearing, or reparse-bearing roots are rejected.
Function LsdjExistingLayoutIsRecognized
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  Push $6
  Push $7
  StrCpy $LsdjSafeLayout 0
  StrCpy $2 0
  StrCpy $3 0
  StrCpy $4 0
  StrCpy $5 0
  StrCpy $6 0

  ClearErrors
  FindFirst $0 $1 "${LSDJ_DATA_ROOT}\*"
  IfErrors lsdj_layout_done
  lsdj_layout_next:
    StrCmp $1 "." lsdj_layout_advance
    StrCmp $1 ".." lsdj_layout_advance
    StrCmp $1 "config" lsdj_layout_config
    StrCmp $1 "data" lsdj_layout_data
    StrCmp $1 "cache" lsdj_layout_cache
    StrCmp $1 "assets" lsdj_layout_assets
    StrCmp $1 "staging" lsdj_layout_staging
    Goto lsdj_layout_close

    lsdj_layout_config:
      StrCpy $2 1
      Goto lsdj_layout_validate_directory
    lsdj_layout_data:
      StrCpy $3 1
      Goto lsdj_layout_validate_directory
    lsdj_layout_cache:
      StrCpy $4 1
      Goto lsdj_layout_validate_directory
    lsdj_layout_assets:
      StrCpy $5 1
      Goto lsdj_layout_validate_directory
    lsdj_layout_staging:
      StrCpy $6 1

    lsdj_layout_validate_directory:
      System::Call 'kernel32::GetFileAttributesW(w "${LSDJ_DATA_ROOT}\$1") i .r7'
      ${If} $7 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
        Goto lsdj_layout_close
      ${EndIf}
      IntOp $7 $7 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
      ${If} $7 <> 0
        Goto lsdj_layout_close
      ${EndIf}
      System::Call 'kernel32::GetFileAttributesW(w "${LSDJ_DATA_ROOT}\$1") i .r7'
      IntOp $7 $7 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
      ${If} $7 = 0
        Goto lsdj_layout_close
      ${EndIf}

    lsdj_layout_advance:
      ClearErrors
      FindNext $0 $1
      IfErrors lsdj_layout_complete
      Goto lsdj_layout_next

  lsdj_layout_complete:
    ${If} $2 = 1
    ${AndIf} $3 = 1
    ${AndIf} $4 = 1
    ${AndIf} $5 = 1
    ${AndIf} $6 = 1
      StrCpy $LsdjSafeLayout 1
    ${EndIf}

  lsdj_layout_close:
    FindClose $0
  lsdj_layout_done:
  Pop $7
  Pop $6
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

Function LsdjInstallTreeIsLinkFree
  Exch $0
  Push $1
  Push $2
  Push $3
  ${If} $LsdjTreeSafe = 0
    Goto lsdj_install_tree_done
  ${EndIf}

  System::Call 'kernel32::GetFileAttributesW(w r0) i .r1'
  ${If} $1 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
    Goto lsdj_install_tree_unsafe
  ${EndIf}
  IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
  ${If} $3 <> 0
    Goto lsdj_install_tree_unsafe
  ${EndIf}
  IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
  ${If} $3 = 0
    Goto lsdj_install_tree_unsafe
  ${EndIf}

  ClearErrors
  FindFirst $1 $2 "$0\*"
  IfErrors lsdj_install_tree_empty_candidate
  lsdj_install_tree_next:
    StrCmp $2 "." lsdj_install_tree_advance
    StrCmp $2 ".." lsdj_install_tree_advance
    System::Call 'kernel32::GetFileAttributesW(w "$0\$2") i .r3'
    ${If} $3 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
      Goto lsdj_install_tree_unsafe_close
    ${EndIf}
    IntOp $3 $3 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
    ${If} $3 <> 0
      Goto lsdj_install_tree_unsafe_close
    ${EndIf}
    System::Call 'kernel32::GetFileAttributesW(w "$0\$2") i .r3'
    IntOp $3 $3 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
    ${If} $3 <> 0
      Push "$0\$2"
      Call LsdjInstallTreeIsLinkFree
      ${If} $LsdjTreeSafe = 0
        Goto lsdj_install_tree_close
      ${EndIf}
    ${EndIf}
    lsdj_install_tree_advance:
      ClearErrors
      FindNext $1 $2
      IfErrors lsdj_install_tree_close
      Goto lsdj_install_tree_next

  lsdj_install_tree_unsafe_close:
    StrCpy $LsdjTreeSafe 0
  lsdj_install_tree_close:
    FindClose $1
    Goto lsdj_install_tree_done
  ; FindFirst reports an error for a plain empty directory. Re-read the entry
  ; itself before accepting that error as an empty leaf, so disappearance,
  ; replacement, and reparse-point races still fail closed.
  lsdj_install_tree_empty_candidate:
    System::Call 'kernel32::GetFileAttributesW(w r0) i .r1'
    ${If} $1 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
      Goto lsdj_install_tree_unsafe
    ${EndIf}
    IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
    ${If} $3 <> 0
      Goto lsdj_install_tree_unsafe
    ${EndIf}
    IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
    ${If} $3 = 0
      Goto lsdj_install_tree_unsafe
    ${EndIf}
    Goto lsdj_install_tree_done
  lsdj_install_tree_unsafe:
    StrCpy $LsdjTreeSafe 0
  lsdj_install_tree_done:
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

Function LsdjDataRootIsEmpty
  Push $0
  Push $1
  StrCpy $LsdjRootEmpty 0
  ClearErrors
  FindFirst $0 $1 "${LSDJ_DATA_ROOT}\*"
  IfErrors lsdj_empty_recheck
  lsdj_empty_next:
    StrCmp $1 "." lsdj_empty_advance
    StrCmp $1 ".." lsdj_empty_advance
    Goto lsdj_empty_close
    lsdj_empty_advance:
      ClearErrors
      FindNext $0 $1
      IfErrors lsdj_empty_confirmed
      Goto lsdj_empty_next
  lsdj_empty_confirmed:
    StrCpy $LsdjRootEmpty 1
  lsdj_empty_close:
    FindClose $0
    Goto lsdj_empty_done
  ; Tauri's SetOutPath creates a genuinely empty root on first install. Accept
  ; the failed enumeration only after the root is still the same plain
  ; directory shape required by the ownership checks.
  lsdj_empty_recheck:
    System::Call 'kernel32::GetFileAttributesW(w "${LSDJ_DATA_ROOT}") i .r0'
    ${If} $0 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
      Goto lsdj_empty_done
    ${EndIf}
    IntOp $1 $0 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
    ${If} $1 <> 0
      Goto lsdj_empty_done
    ${EndIf}
    IntOp $1 $0 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
    ${If} $1 = 0
      Goto lsdj_empty_done
    ${EndIf}
    StrCpy $LsdjRootEmpty 1
  lsdj_empty_done:
  Pop $1
  Pop $0
FunctionEnd

; installerHooks is included before Tauri declares any of its sections. NSIS
; executes sections in declaration order, so this hidden, always-selected probe
; observes the root before Tauri's Install section executes SetOutPath. It does
; not create or mark anything: PREINSTALL uses this captured state after
; SetOutPath and immediately revalidates before establishing ownership.
Section -LsdjProbeDataRootBeforeTauri
  StrCpy $LsdjInstallRootState 0
  Call LsdjCanonicalDataRootIsValid
  ${If} $LsdjCanonicalRootSafe != 1
    Abort "Refusing to install: the LSDJ data root is not the exact LocalAppData target."
  ${EndIf}

  System::Call 'kernel32::GetFileAttributesW(w "${LSDJ_DATA_ROOT}") i .R8'
  ${If} $R8 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
    Goto lsdj_probe_absent
  ${EndIf}
  IntOp $R7 $R8 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
  ${If} $R7 <> 0
    Abort "Refusing to install into a junction, symbolic link, or reparse point at ${LSDJ_DATA_ROOT}."
  ${EndIf}
  IntOp $R7 $R8 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
  ${If} $R7 = 0
    Abort "Refusing to install: ${LSDJ_DATA_ROOT} exists but is not a directory."
  ${EndIf}

  ClearErrors
  FindFirst $R7 $R8 "${LSDJ_DATA_MARKER}"
  IfErrors lsdj_probe_legacy
  FindClose $R7
  Call LsdjDataMarkerIsValid
  ${If} $LsdjMarkerSafe != 1
    Abort "Refusing to install: the LSDJ ownership marker is invalid or is a reparse point."
  ${EndIf}
  StrCpy $LsdjInstallRootState 3
  Goto lsdj_probe_done

  lsdj_probe_legacy:
    Call LsdjExistingLayoutIsRecognized
    StrCpy $LsdjTreeSafe 1
    ${If} $LsdjSafeLayout = 1
      Push "${LSDJ_DATA_ROOT}"
      Call LsdjInstallTreeIsLinkFree
    ${EndIf}
    ${If} $LsdjSafeLayout != 1
    ${OrIf} $LsdjTreeSafe != 1
      Abort "Refusing to claim a pre-existing foreign or unrecognized directory at ${LSDJ_DATA_ROOT}."
    ${EndIf}
    StrCpy $LsdjInstallRootState 2
    Goto lsdj_probe_done

  lsdj_probe_absent:
    StrCpy $LsdjInstallRootState 1
  lsdj_probe_done:
SectionEnd

; Validate the exact root and marker together. GetFileAttributesW reports the
; root entry itself, so directory junctions and symbolic links are rejected.
Function un.LsdjOwnedDataRootIsSafe
  Push $R7
  Push $R8
  StrCpy $LsdjOwnedRootSafe 0
  Call un.LsdjCanonicalDataRootIsValid
  ${If} $LsdjCanonicalRootSafe != 1
    Goto lsdj_owned_root_done
  ${EndIf}
  System::Call 'kernel32::GetFileAttributesW(w "${LSDJ_DATA_ROOT}") i .R8'
  ${If} $R8 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
    Goto lsdj_owned_root_done
  ${EndIf}
  IntOp $R7 $R8 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
  ${If} $R7 <> 0
    Goto lsdj_owned_root_done
  ${EndIf}
  IntOp $R7 $R8 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
  ${If} $R7 = 0
    Goto lsdj_owned_root_done
  ${EndIf}
  Call un.LsdjDataMarkerIsValid
  ${If} $LsdjMarkerSafe = 1
    StrCpy $LsdjOwnedRootSafe 1
  ${EndIf}

  lsdj_owned_root_done:
  Pop $R8
  Pop $R7
FunctionEnd

; Walk the tree without traversing a reparse point. Purge is refused before
; GetSize if any link is present, so the disclosed size covers only the tree
; that the safe deleter is allowed to remove.
Function un.LsdjTreeIsLinkFree
  Exch $0
  Push $1
  Push $2
  Push $3
  ${If} $LsdjTreeSafe = 0
    Goto lsdj_tree_done
  ${EndIf}

  System::Call 'kernel32::GetFileAttributesW(w r0) i .r1'
  ${If} $1 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
    Goto lsdj_tree_unsafe
  ${EndIf}
  IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
  ${If} $3 <> 0
    Goto lsdj_tree_unsafe
  ${EndIf}
  IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
  ${If} $3 = 0
    Goto lsdj_tree_unsafe
  ${EndIf}

  ClearErrors
  FindFirst $1 $2 "$0\*"
  IfErrors lsdj_tree_empty_candidate
  lsdj_tree_next:
    StrCmp $2 "." lsdj_tree_advance
    StrCmp $2 ".." lsdj_tree_advance
    System::Call 'kernel32::GetFileAttributesW(w "$0\$2") i .r3'
    ${If} $3 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
      Goto lsdj_tree_unsafe_close
    ${EndIf}
    IntOp $3 $3 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
    ${If} $3 <> 0
      Goto lsdj_tree_unsafe_close
    ${EndIf}
    System::Call 'kernel32::GetFileAttributesW(w "$0\$2") i .r3'
    IntOp $3 $3 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
    ${If} $3 <> 0
      Push "$0\$2"
      Call un.LsdjTreeIsLinkFree
      ${If} $LsdjTreeSafe = 0
        Goto lsdj_tree_close
      ${EndIf}
    ${EndIf}
    lsdj_tree_advance:
      ClearErrors
      FindNext $1 $2
      IfErrors lsdj_tree_close
      Goto lsdj_tree_next

  lsdj_tree_unsafe_close:
    StrCpy $LsdjTreeSafe 0
  lsdj_tree_close:
    FindClose $1
    Goto lsdj_tree_done
  lsdj_tree_empty_candidate:
    System::Call 'kernel32::GetFileAttributesW(w r0) i .r1'
    ${If} $1 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
      Goto lsdj_tree_unsafe
    ${EndIf}
    IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
    ${If} $3 <> 0
      Goto lsdj_tree_unsafe
    ${EndIf}
    IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
    ${If} $3 = 0
      Goto lsdj_tree_unsafe
    ${EndIf}
    Goto lsdj_tree_done
  lsdj_tree_unsafe:
    StrCpy $LsdjTreeSafe 0
  lsdj_tree_done:
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; Recursive deletion mirrors the validation walk and refuses any reparse entry
; observed during deletion. It never invokes NSIS's broad recursive-directory
; removal and never descends through a junction or symbolic link, including one
; introduced after confirmation.
Function un.LsdjDeleteTreeWithoutLinks
  Exch $0
  Push $1
  Push $2
  Push $3
  ${If} $LsdjDeleteFailure = 1
    Goto lsdj_delete_done
  ${EndIf}

  System::Call 'kernel32::GetFileAttributesW(w r0) i .r1'
  ${If} $1 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
    Goto lsdj_delete_failed
  ${EndIf}
  IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
  ${If} $3 <> 0
    Goto lsdj_delete_failed
  ${EndIf}
  IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
  ${If} $3 = 0
    Goto lsdj_delete_failed
  ${EndIf}

  ClearErrors
  FindFirst $1 $2 "$0\*"
  IfErrors lsdj_delete_empty_candidate
  lsdj_delete_next:
    StrCmp $2 "." lsdj_delete_advance
    StrCmp $2 ".." lsdj_delete_advance
    System::Call 'kernel32::GetFileAttributesW(w "$0\$2") i .r3'
    ${If} $3 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
      Goto lsdj_delete_failed_close
    ${EndIf}
    IntOp $3 $3 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
    ${If} $3 <> 0
      Goto lsdj_delete_failed_close
    ${EndIf}
    System::Call 'kernel32::GetFileAttributesW(w "$0\$2") i .r3'
    IntOp $3 $3 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
    ${If} $3 <> 0
      Push "$0\$2"
      Call un.LsdjDeleteTreeWithoutLinks
      ${If} $LsdjDeleteFailure = 1
        Goto lsdj_delete_close
      ${EndIf}
    ${Else}
      ClearErrors
      Delete "$0\$2"
      IfErrors lsdj_delete_failed_close
    ${EndIf}
    lsdj_delete_advance:
      ClearErrors
      FindNext $1 $2
      IfErrors lsdj_delete_close
      Goto lsdj_delete_next

  lsdj_delete_failed_close:
    StrCpy $LsdjDeleteFailure 1
  lsdj_delete_close:
    FindClose $1
    ${If} $LsdjDeleteFailure = 0
      ClearErrors
      RMDir "$0"
      IfErrors lsdj_delete_failed
    ${EndIf}
    Goto lsdj_delete_done
  lsdj_delete_empty_candidate:
    System::Call 'kernel32::GetFileAttributesW(w r0) i .r1'
    ${If} $1 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
      Goto lsdj_delete_failed
    ${EndIf}
    IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
    ${If} $3 <> 0
      Goto lsdj_delete_failed
    ${EndIf}
    IntOp $3 $1 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
    ${If} $3 = 0
      Goto lsdj_delete_failed
    ${EndIf}
    ClearErrors
    RMDir "$0"
    IfErrors lsdj_delete_failed
    Goto lsdj_delete_done
  lsdj_delete_failed:
    StrCpy $LsdjDeleteFailure 1
  lsdj_delete_done:
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

!macro NSIS_HOOK_PREINSTALL
  ; SetOutPath has now created a root that the early section proved absent, or
  ; selected an existing root whose ownership/layout the early section proved.
  ; Revalidate that captured state before writing anything into the directory.
  Call LsdjCanonicalDataRootIsValid
  ${If} $LsdjCanonicalRootSafe != 1
    Abort "Refusing to install: the LSDJ data root is not the exact LocalAppData target."
  ${EndIf}

  System::Call 'kernel32::GetFileAttributesW(w "${LSDJ_DATA_ROOT}") i .R8'
  ${If} $R8 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
    ${If} $LsdjInstallRootState != 1
      Goto lsdj_install_root_missing
    ${EndIf}
    ; A custom /D install location means Tauri's SetOutPath did not create the
    ; separate LocalAppData root. The early section proved it absent; create it
    ; now, fail if another entry won the race, and validate the new entry below.
    ClearErrors
    CreateDirectory "${LSDJ_DATA_ROOT}"
    IfErrors lsdj_install_root_create_failed
    System::Call 'kernel32::GetFileAttributesW(w "${LSDJ_DATA_ROOT}") i .R8'
    ${If} $R8 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
      Goto lsdj_install_root_create_failed
    ${EndIf}
  ${EndIf}
  IntOp $R7 $R8 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
  ${If} $R7 <> 0
    Abort "Refusing to install into a junction, symbolic link, or reparse point at ${LSDJ_DATA_ROOT}."
  ${EndIf}
  IntOp $R7 $R8 & ${LSDJ_FILE_ATTRIBUTE_DIRECTORY}
  ${If} $R7 = 0
    Abort "Refusing to install: ${LSDJ_DATA_ROOT} exists but is not a directory."
  ${EndIf}

  ${If} $LsdjInstallRootState = 1
    Call LsdjDataRootIsEmpty
    ${If} $LsdjRootEmpty != 1
      Abort "Refusing to install: the newly created LSDJ root changed before ownership was established."
    ${EndIf}
    Goto lsdj_write_data_marker
  ${ElseIf} $LsdjInstallRootState = 2
    Call LsdjExistingLayoutIsRecognized
    StrCpy $LsdjTreeSafe 1
    ${If} $LsdjSafeLayout = 1
      Push "${LSDJ_DATA_ROOT}"
      Call LsdjInstallTreeIsLinkFree
    ${EndIf}
    ${If} $LsdjSafeLayout != 1
    ${OrIf} $LsdjTreeSafe != 1
      Abort "Refusing to install: the recognized LSDJ layout changed before ownership was established."
    ${EndIf}
    Goto lsdj_write_data_marker
  ${ElseIf} $LsdjInstallRootState = 3
    Call LsdjDataMarkerIsValid
    ${If} $LsdjMarkerSafe != 1
      Abort "Refusing to install: LSDJ ownership changed after the early root probe."
    ${EndIf}
    Goto lsdj_install_root_ready
  ${Else}
    Abort "Refusing to install: the LSDJ data root was not safely classified before SetOutPath."
  ${EndIf}

  lsdj_write_data_marker:
    Call LsdjCreateDataMarker
    ${If} $LsdjMarkerSafe != 1
      Goto lsdj_marker_create_failed
    ${EndIf}
    Goto lsdj_install_root_ready

  lsdj_marker_create_failed:
    Delete "${LSDJ_DATA_MARKER_NEW}"
    Abort "Refusing to install: a plain LSDJ ownership marker could not be established safely."
  lsdj_install_root_missing:
    Abort "Refusing to install: Tauri did not create the LSDJ root classified by the early ownership probe."
  lsdj_install_root_create_failed:
    Abort "Refusing to install: the separately located LSDJ data root could not be created safely."
  lsdj_install_root_ready:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; A second check ensures copying never silently replaced the owned root or
  ; marker. Do not rewrite or repair either safety boundary here.
  Call LsdjCanonicalDataRootIsValid
  ${If} $LsdjCanonicalRootSafe != 1
    Abort "LSDJ installation did not retain the exact LocalAppData root."
  ${EndIf}
  System::Call 'kernel32::GetFileAttributesW(w "${LSDJ_DATA_ROOT}") i .R8'
  ${If} $R8 = ${LSDJ_INVALID_FILE_ATTRIBUTES}
    Abort "LSDJ installation lost its LocalAppData root."
  ${EndIf}
  IntOp $R7 $R8 & ${LSDJ_FILE_ATTRIBUTE_REPARSE_POINT}
  ${If} $R7 <> 0
    Abort "LSDJ installation encountered an unsafe data-root reparse point."
  ${EndIf}
  Call LsdjDataMarkerIsValid
  ${If} $LsdjMarkerSafe != 1
    Abort "LSDJ installation did not retain its plain ownership marker."
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; The normal checkbox is deliberately unchecked by default. Silent removal
  ; must name the destructive option; /S alone always preserves user assets.
  StrCpy $LsdjDeleteData 0
  StrCpy $LsdjDataRemovalFailed 0
  ClearErrors
  ${GetOptions} $CMDLINE "/PURGE-LSDJ-DATA" $R8
  ${IfNot} ${Errors}
    StrCpy $DeleteAppDataCheckboxState 1
  ${EndIf}

  ${If} $DeleteAppDataCheckboxState = 1
    Call un.LsdjOwnedDataRootIsSafe
    StrCpy $LsdjTreeSafe 1
    ${If} $LsdjOwnedRootSafe = 1
      Push "${LSDJ_DATA_ROOT}"
      Call un.LsdjTreeIsLinkFree
    ${EndIf}
    ${If} $LsdjOwnedRootSafe != 1
    ${OrIf} $LsdjTreeSafe != 1
      StrCpy $LsdjDataRemovalFailed 1
      DetailPrint "Refusing to remove LSDJ data: exact ownership or reparse-safety validation failed at ${LSDJ_DATA_ROOT}"
      StrCpy $DeleteAppDataCheckboxState 0
      ${IfNot} ${Silent}
        MessageBox MB_ICONSTOP|MB_OK "LSDJ will preserve the data at:$\n${LSDJ_DATA_ROOT}$\n$\nThe root or ownership marker is invalid, or the tree contains a reparse point, so automatic removal is unsafe."
      ${EndIf}
      ; Stop before Tauri's ordinary payload deletion too: the application and
      ; data share a root in the default layout, so continuing after a root
      ; ownership failure could make even narrow file deletions unsafe.
      SetErrorLevel 2
      Abort "Refusing unsafe LSDJ data removal."
    ${EndIf}

    ; GetSize reports KiB. The link-free walk above prevents it from traversing
    ; junctions while measuring the exact tree that may be removed.
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

  ; Tauri's generic checkbox handling recursively removes undisclosed bundle-ID
  ; APPDATA roots. Preserve the choice separately and suppress that deletion.
  StrCpy $LsdjDeleteData $DeleteAppDataCheckboxState
  StrCpy $DeleteAppDataCheckboxState 0
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $LsdjDeleteData = 1
  ${AndIf} $UpdateMode <> 1
    !ifdef LSDJ_CI_ADVERSARIAL_TESTS
      ; Unsigned hosted-CI installers can pause after confirmation/core removal
      ; so the test can deterministically replace the marker before this second
      ; validation. This branch is absent from release installers.
      ClearErrors
      ${GetOptions} $CMDLINE "/LSDJ-CI-PAUSE-BEFORE-PURGE" $R8
      ${IfNot} ${Errors}
        FileOpen $R7 "$TEMP\lsdj-ci-before-purge.ready" w
        FileWrite $R7 "ready"
        FileClose $R7
        Sleep 5000
        Delete "$TEMP\lsdj-ci-before-purge.ready"
      ${EndIf}
    !endif

    ; Revalidate canonical path, root, marker, and every tree entry immediately
    ; before deletion. The deletion walk performs the same checks again.
    Call un.LsdjOwnedDataRootIsSafe
    StrCpy $LsdjTreeSafe 1
    ${If} $LsdjOwnedRootSafe = 1
      Push "${LSDJ_DATA_ROOT}"
      Call un.LsdjTreeIsLinkFree
    ${EndIf}
    ${If} $LsdjOwnedRootSafe = 1
    ${AndIf} $LsdjTreeSafe = 1
      StrCpy $LsdjDeleteFailure 0
      Push "${LSDJ_DATA_ROOT}"
      Call un.LsdjDeleteTreeWithoutLinks
      ${If} $LsdjDeleteFailure = 0
        ; Match Tauri's explicit-data-removal registry cleanup without invoking
        ; its generic recursive directory deletion.
        DeleteRegKey SHCTX "${MANUPRODUCTKEY}"
        DeleteRegKey /ifempty SHCTX "${MANUKEY}"
        DeleteRegValue HKCU "${MANUPRODUCTKEY}" "Installer Language"
        DeleteRegKey /ifempty HKCU "${MANUPRODUCTKEY}"
        DeleteRegKey /ifempty HKCU "${MANUKEY}"
      ${Else}
        DetailPrint "LSDJ data removal stopped because a reparse point or filesystem error was observed"
        StrCpy $LsdjDataRemovalFailed 1
      ${EndIf}
    ${Else}
      DetailPrint "LSDJ data was preserved because exact ownership or reparse-safety validation changed"
      StrCpy $LsdjDataRemovalFailed 1
    ${EndIf}
  ${EndIf}
  ${If} $LsdjDataRemovalFailed = 1
    SetErrorLevel 2
  ${EndIf}
!macroend
