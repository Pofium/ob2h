.globl crypto_init
.globl crypto_start

.text
crypto_init:
    call    setup_stack
    ret

crypto_start:
    call    crypto_init
    call    setup_stack
    ret

setup_stack:
    push    %rbp
    ret

.section .rodata
msg_str:
    .string "ob2h"

.equ    AES_BLOCK, 16

.macro  SAVE_REGS
    push    %rbp
.endm
