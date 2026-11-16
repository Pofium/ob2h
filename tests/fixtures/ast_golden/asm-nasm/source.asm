%include "helpers.asm"

section .text
global nasm_entry

nasm_entry:
    call    nasm_helper
    ret

nasm_helper:
    call    print_char
    ret

print_char:
    ret

section .data
answer dq 42
magic equ 7

%define LIMIT 100

%macro PUSH_ALL 0
    push rax
%endmacro
