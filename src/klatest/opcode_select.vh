// Can be read by assembler, so format is fixed. 
// Opcode must be first word in comment.
// Format codes: R=register, V=value, RR=two registers, RRV=two registers+value

/* Macro definition
$POPALL POP A / POP B / POP C
$PUSHALL PUSH A / PUSH B / PUSH C
$WAIT DELAYV %1 / DELAYV %2
$TESTM NOP / NOP / NOP
$TESTM2 NOP
$IMBED1 DELAYV 0xFFFF
$IMBED3 $PUSHALL / $IMBED1
$UART_STRING PUSH A / PUSH B / SETR A %1
*/

task t_opcode_select;
   begin
      casez (w_opcode[15:0])

         //=====================================================================
         // Register control 0xxx
         //=====================================================================
         16'h01??: t_copy_regs;                             // COPY Copy second register into first
         16'h02??: t_and_regs;                              // AND And registers, result in first register
         16'h03??: t_or_regs;                               // OR Or registers, result in first register
         16'h04??: t_xor_regs;                              // XOR XOR registers, result in first register
         16'h05??: t_compare_regs;                          // CMPRR Compare registers, sets equal/less/sign flags
         16'h06??: t_add_regs;                              // ADDRR Add registers, result in first register
         16'h07??: t_minus_regs;                            // MINUSRR Minus registers, result in first register
         
         16'h080?: t_set_reg(w_var1);                       // SETR Set register to a value
         16'h081?: t_add_value(w_var1);                     // ADDV Add value to register
         16'h082?: t_minus_value(w_var1);                   // MINUSV Subtract value from register
         16'h083?: t_compare_reg_value(w_var1);             // CMPRV Compare register to value, sets equal/less/sign flags
         16'h084?: t_inc_reg;                               // INCR Increment register
         16'h085?: t_dec_reg;                               // DECR Decrement register
         16'h086?: t_and_reg_value(w_var1);                 // ANDV AND register with value, result in register
         16'h087?: t_or_reg_value(w_var1);                  // ORV OR register with value, result in register
         16'h088?: t_xor_reg_value(w_var1);                 // XORV XOR register with value, result in register
         16'h089?: t_set_reg_flags;                         // SETFR Set register to flags value
         16'h08A?: t_negate_reg;                            // NEGR Set register 2's complement
         16'h08B?: t_abs_reg;                               // ABSR Absolute value of register
         16'h08C?: t_sign_extend_byte;                      // SEXTB Sign extend byte to 32 bits
         16'h08D?: t_left_shift_reg;                        // SHLR Left shift register by 1
         16'h08E?: t_right_shift_reg;                       // SHRR Right shift register by 1 (logical)
         16'h08F?: t_left_shift_a_reg;                      // SHLAR Left shift arithmetical register
         16'h090?: t_right_shift_a_reg;                     // SHRAR Right shift arithmetical register by 1
         16'h091?: t_left_shift_n(w_var1);                  // SHLV Left shift register by N bits
         16'h092?: t_right_shift_n(w_var1);                 // SHRV Right shift register by N bits (logical)
         16'h093?: t_right_shift_a_n(w_var1);               // SHRAV Right shift arithmetical by N bits
         16'h094?: t_sign_extend_half;                      // SEXTH Sign extend halfword to 32 bits
         16'h095?: t_zero_extend_byte;                      // ZEXTB Zero extend byte to 32 bits
         16'h096?: t_zero_extend_half;                      // ZEXTH Zero extend halfword to 32 bits
         16'h097?: t_byte_swap;                             // BSWAP Byte swap (endian conversion)
         // MINRR/MAXRR moved to A0??-A3?? for full two-register encoding

         //=====================================================================
         // Bit manipulation 0A0x-0AFx
         //=====================================================================
         16'h0A0?: t_bit_set_value(w_var1);                 // BSET Set bit N in register
         16'h0A1?: t_bit_clear_value(w_var1);               // BCLR Clear bit N in register
         16'h0A2?: t_bit_toggle_value(w_var1);              // BTGL Toggle bit N in register
         16'h0A3?: t_bit_test_value(w_var1);                // BTST Test bit N, result in zero flag
         // BSETRR/BCLRRR/BTGLRR/BTSTRR moved to B2??-B5?? for full two-register encoding
         16'h0A8?: t_popcnt;                                // POPCNT Population count (count 1 bits)
         16'h0A9?: t_clz;                                   // CLZ Count leading zeros
         16'h0AA?: t_ctz;                                   // CTZ Count trailing zeros
         16'h0AB?: t_bit_reverse;                           // BITREV Reverse all bits in register
         16'h0AC?: t_extract_bits(w_var1);                  // BEXTR Extract bit field (pos:8, len:8)
         16'h0AD?: t_deposit_bits(w_var1);                  // BDEP Deposit bit field
         
         //=====================================================================
         // Hardware multiply/divide by value (using DSP)
         //=====================================================================
         // MULRR/DIVRR etc. moved to 80??-87?? for full two-register encoding
         16'h0B8?: t_mul_value_hw(w_var1);                  // MULV Multiply register by value (signed)
         16'h0B9?: t_div_value_hw(w_var1);                  // DIVV Divide register by value (signed)
         16'h0BA?: t_mod_value_hw(w_var1);                  // MODV Modulo register by value (signed)

         //=====================================================================
         // Indexed memory access 0C0x-0CFx
         //=====================================================================
         16'h0C??: t_load_indexed(w_var1);                   // LDIDX Load indexed: first = mem[second + var1]
         16'h0D??: t_store_indexed(w_var1);                  // STIDX Store indexed: mem[second + var1] = first
         16'h0E??: t_load_indexed_reg(w_var1);               // LDIDXR Load indexed: first = mem[second + reg[var1]]

         // CMPLTRR etc. moved to 90??-97?? for full two-register encoding

         //=====================================================================
         // Rotate operations 0F8x-0FDx
         //=====================================================================
         16'h0F8?: t_rotate_left;                           // ROLR Rotate left by 1
         16'h0F9?: t_rotate_right;                          // RORR Rotate right by 1
         16'h0FA?: t_rotate_left_carry;                     // ROLCR Rotate left through carry
         16'h0FB?: t_rotate_right_carry;                    // RORCR Rotate right through carry
         16'h0FC?: t_rotate_left_n(w_var1);                 // ROLV Rotate left by N bits
         16'h0FD?: t_rotate_right_n(w_var1);                // RORV Rotate right by N bits
         // ROLRR/RORRR moved to B0??-B1?? for full two-register encoding

         //=====================================================================
         // Flow control 1xxx
         //=====================================================================
         16'h1000: t_cond_jump(w_var1, 1'b1);               // JMP Jump
         16'h1001: t_cond_jump(w_var1, r_zero_flag);        // JMPZ Jump if zero
         16'h1002: t_cond_jump(w_var1, !r_zero_flag);       // JMPNZ Jump if not zero
         16'h1003: t_cond_jump(w_var1, r_equal_flag);       // JMPE Jump if equal
         16'h1004: t_cond_jump(w_var1, !r_equal_flag);      // JMPNE Jump if not equal
         16'h1005: t_cond_jump(w_var1, r_carry_flag);       // JMPC Jump if carry
         16'h1006: t_cond_jump(w_var1, !r_carry_flag);      // JMPNC Jump if not carry
         16'h1007: t_cond_jump(w_var1, r_overflow_flag);    // JMPO Jump if overflow
         16'h1008: t_cond_jump(w_var1, !r_overflow_flag);   // JMPNO Jump if not overflow
         16'h1009: t_cond_call(w_var1, 1'b1);               // CALL Call function
         16'h100A: t_cond_call(w_var1, r_zero_flag);        // CALLZ Call if zero
         16'h100B: t_cond_call(w_var1, !r_zero_flag);       // CALLNZ Call if not zero
         16'h100C: t_cond_call(w_var1, r_equal_flag);       // CALLE Call if equal
         16'h100D: t_cond_call(w_var1, !r_equal_flag);      // CALLNE Call if not equal
         16'h100E: t_cond_call(w_var1, r_carry_flag);       // CALLC Call if carry
         16'h100F: t_cond_call(w_var1, !r_carry_flag);      // CALLNC Call if not carry
         16'h1010: t_cond_call(w_var1, r_overflow_flag);    // CALLO Call if overflow
         16'h1011: t_cond_call(w_var1, !r_overflow_flag);   // CALLNO Call if not overflow
         16'h1012: t_ret;                                   // RET Return from call
         16'h1013: t_cond_jump(w_var1, r_sign_flag);        // JMPS Jump if sign (negative)
         16'h1014: t_cond_jump(w_var1, !r_sign_flag);       // JMPNS Jump if not sign (positive)
         // Signed comparison jumps (use after CMPLTRR/CMPRR etc.)
         16'h1015: t_cond_jump(w_var1, r_less_flag);        // JMPLT Jump if less-than (signed)
         16'h1016: t_cond_jump(w_var1, r_less_flag | r_equal_flag); // JMPLE Jump if less-or-equal
         16'h1017: t_cond_jump(w_var1, !r_less_flag & !r_equal_flag); // JMPGT Jump if greater-than
         16'h1018: t_cond_jump(w_var1, !r_less_flag);       // JMPGE Jump if greater-or-equal
         16'h102?: t_jump_reg;                              // JMPR Jump to address in register

         //=====================================================================
         // SPI LCD Control 2xxx
         //=====================================================================
         16'h200?: spi_dc_write_command_reg;                // CDCDMR LCD command with register
         16'h201?: spi_dc_data_command_reg;                 // LCDDATAR LCD data with register
         16'h2021: spi_dc_write_command_value(w_var1);      // LCDCMDV LCD write command value
         16'h2022: spi_dc_write_data_value(w_var1);         // LCDDATAV LCD data with value
         16'h2023: t_lcd_reset_value(w_var1);               // LCD Reset line

         //=====================================================================
         // Board LED and Switch 3xxx
         //=====================================================================
         16'h300?: t_led_reg;                               // LEDR set LEDs with register
         16'h301?: t_get_switch_reg;                        // SWR Get switch status into register
         16'h302?: t_7_seg1_reg;                            // 7SEG1R Set 7 Seg 1 to register
         16'h303?: t_7_seg2_reg;                            // 7SEG2R Set 7 Seg 2 to register
         16'h304?: t_7_seg_reg;                             // 7SEGR Set 7 Seg to register
         16'h305?: t_led_rgb1_reg;                          // RGB1R RGB 1 from register
         16'h306?: t_led_rgb2_reg;                          // RGB2R RGB 2 from register
         16'h3070: t_led_value(w_var1);                     // LEDV Set LED to value
         16'h3071: t_7_seg1_value(w_var1);                  // 7SEG1V Set 7 Seg 1 to value
         16'h3072: t_7_seg2_value(w_var1);                  // 7SEG2V Set 7 Seg 2 to value
         16'h3073: t_7_seg_blank;                           // 7SEGBLANK Blank 7 Seg
         16'h3074: t_led_rgb1_value(w_var1);                // RGB1V RGB 1 from value
         16'h3075: t_led_rgb2_value(w_var1);                // RGB2V RGB 2 from value

         //=====================================================================
         // Stack control 4xxx
         //=====================================================================
         16'h400?: t_stack_push_reg;                        // PUSH Push register onto stack
         16'h401?: t_stack_pop_reg;                         // POP Pop stack into register
         16'h4020: t_stack_push_value(w_var1);              // PUSHV Push value onto stack

         //=====================================================================
         // Communication 5xxx
         //=====================================================================
         16'h5000: t_test_message;                          // TESTMSG send test UART message
         16'h5001: t_tx_newline;                            // NEWLINE send UART newline
         16'h5002: t_tx_value_of_mem(w_var1);               // TXMEM send 8 bytes value of memory location
         16'h5003: t_tx_string_at_mem(w_var1);              // TXSTRMEM send string at memory
         16'h501?: t_tx_reg;                                // TXR send 8 bytes reg value in message
         16'h502?: t_tx_value_of_mem_at_reg;                // TXMEMR send 8 bytes value at memory of register value
         16'h503?: t_tx_char_from_reg_value;                // TXCHARMEMR send char at memory from register value
         16'h504?: t_tx_string_at_reg;                      // TXSTRMEMR send string at memory location from register

         //=====================================================================
         // CPU Setting 6xxx
         //=====================================================================
         16'h60??: t_set_interrupt_regs;                    // INTSETRR Set interrupt from registers

         //=====================================================================
         // Memory actions 7xxx
         //=====================================================================
         16'h70??: t_set_mem_from_reg_reg;                  // MEMSETRR mem[second] = first
         16'h71??: t_set_reg_from_mem_reg;                  // MEMREADRR first = mem[second]
         16'h720?: t_set_mem_from_value_reg(w_var1);        // MEMSETR Set memory from value
         16'h721?: t_set_reg_from_mem_value(w_var1);        // MEMREADR Set register from memory at value
         16'h73??: t_store_indexed_reg(w_var1);              // STIDXR Store indexed: mem[second + reg[var1]] = first

         //=====================================================================
         // Hardware multiply/divide registers 8xxx (using DSP)
         //=====================================================================
         16'h80??: t_mul_regs_hw;                           // MULRR Multiply first * second (signed), result in first
         16'h81??: t_mulu_regs_hw;                          // MULURR Multiply first * second (unsigned), result in first
         16'h82??: t_mulh_regs_hw;                          // MULHRR Multiply high word first * second (signed), result in first
         16'h83??: t_mulhu_regs_hw;                         // MULHURR Multiply high word first * second (unsigned), result in first
         16'h84??: t_div_regs_hw;                           // DIVRR Divide first / second (signed), result in first
         16'h85??: t_divu_regs_hw;                          // DIVURR Divide first / second (unsigned), result in first
         16'h86??: t_mod_regs_hw;                           // MODRR Modulo first % second (signed), result in first
         16'h87??: t_modu_regs_hw;                          // MODURR Modulo first % second (unsigned), result in first

         //=====================================================================
         // Extended comparison 9xxx (two registers)
         //=====================================================================
         16'h90??: t_cmp_lt_regs;                           // CMPLTRR Compare first < second (signed), sets less/equal flags
         16'h91??: t_cmp_le_regs;                           // CMPLERR Compare first <= second (signed), sets less/equal flags
         16'h92??: t_cmp_gt_regs;                           // CMPGTRR Compare first > second (signed), sets less/equal flags
         16'h93??: t_cmp_ge_regs;                           // CMPGERR Compare first >= second (signed), sets less/equal flags
         16'h94??: t_cmp_ult_regs;                          // CMPULTRR Compare first < second (unsigned), sets carry/equal flags
         16'h95??: t_cmp_ule_regs;                          // CMPULERR Compare first <= second (unsigned), sets carry/equal flags
         16'h96??: t_cmp_ugt_regs;                          // CMPUGTRR Compare first > second (unsigned), sets carry/equal flags
         16'h97??: t_cmp_uge_regs;                          // CMPUGERR Compare first >= second (unsigned), sets carry/equal flags

         //=====================================================================
         // Min/Max Axxx (two registers)
         //=====================================================================
         16'hA0??: t_min_regs;                              // MINRR Minimum of first and second (signed), result in first
         16'hA1??: t_max_regs;                              // MAXRR Maximum of first and second (signed), result in first
         16'hA2??: t_minu_regs;                             // MINURR Minimum of first and second (unsigned), result in first
         16'hA3??: t_maxu_regs;                             // MAXURR Maximum of first and second (unsigned), result in first

         //=====================================================================
         // Rotate by register Bxxx (two registers)
         //=====================================================================
         16'hB0??: t_rotate_left_reg;                       // ROLRR Rotate first left by second bits
         16'hB1??: t_rotate_right_reg;                      // RORRR Rotate first right by second bits

         //=====================================================================
         // Bit manipulation by register Bxxx (two registers)
         //=====================================================================
         16'hB2??: t_bit_set_reg;                           // BSETRR Set bit in first (bit number in second)
         16'hB3??: t_bit_clear_reg;                         // BCLRRR Clear bit in first (bit number in second)
         16'hB4??: t_bit_toggle_reg;                        // BTGLRR Toggle bit in first (bit number in second)
         16'hB5??: t_bit_test_reg;                          // BTSTRR Test bit in first (bit number in second), result in zero flag

         //=====================================================================
         // Other Fxxx
         //=====================================================================
         16'hF00?: t_delay_reg;                             // DELAYR Delay by register
         16'hF010: t_nop;                                   // NOP No operation
         16'hF011: t_halt;                                  // HALT Freeze and hang
         16'hF012: t_reset;                                 // RESET Reset
         16'hF013: t_delay(w_var1);                         // DELAYV Delay by value

         default: begin
            r_SM <= HCF_1;  // Halt and catch fire error 1
            r_error_code <= ERR_INV_OPCODE;
         end
      endcase
   end
endtask