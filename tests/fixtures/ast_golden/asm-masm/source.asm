.MODEL SMALL
.CODE

PUBLIC win_entry

win_entry PROC
    call    win_helper
    ret
win_entry ENDP

win_helper PROC
    call    win_tail
    ret
win_helper ENDP

win_tail PROC
    ret
win_tail ENDP

.DATA
buffer dw 0
limit_v EQU 64

MYMACRO MACRO
    nop
ENDM

END
