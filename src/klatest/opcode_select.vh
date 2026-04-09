// Can be read by assembler, so format is fixed.
// Opcode must be first word in comment.
// Format codes: R=register, V=value, RRR=three registers, RRV=two registers+value
//
// 32-bit opcode format:
//   [31:16] operation code  (identifies the instruction)
//   [15:12] always 0x0      (fixed zero nibble)
//   [11:8]  rd              (destination register)
//   [7:4]   rs1             (source register 1)
//   [3:0]   rs2             (source register 2)
//
// Legacy/non-ALU instructions use: 32'h0000_XXXX (upper 16 bits = 0)
//   [15:12] primary opcode class
//   [11:8]  secondary opcode
//   [7:4]   register 1
//   [3:0]   register 2
//
// 3-register ALU instructions use: 32'hNNNN_0???
//   [31:16] = operation code (non-zero)
//   [15:12] = 0x0 (fixed)
//   [11:8]  = rd  (destination)
//   [7:4]   = rs1 (source 1)
//   [3:0]   = rs2 (source 2)

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
      casez (w_opcode[31:0])

         //=====================================================================
         // 3-register ALU: basic arithmetic/logic  (32'h0001_0??? to 32'h0007_0???)
         // [31:16]=op, [15:12]=0, [11:8]=rd, [7:4]=rs1, [3:0]=rs2
         //=====================================================================
         32'h0001_0???: t_addr3;                               // ADDR RRR rd=rs1+rs2
         32'h0002_0???: t_subr3;                               // SUBR RRR rd=rs1-rs2
         32'h0003_0???: t_andr3;                               // ANDR RRR rd=rs1&rs2
         32'h0004_0???: t_orr3;                                // ORR RRR rd=rs1|rs2
         32'h0005_0???: t_xorr3;                               // XORR RRR rd=rs1^rs2

         //=====================================================================
         // 3-register multiply/divide  (32'h0010_0??? to 32'h0017_0???)
         //=====================================================================
         32'h0010_0???: t_mul_regs_hw;                         // MULR RRR rd=rs1*rs2 signed lo
         32'h0011_0???: t_mulu_regs_hw;                        // MULUR RRR rd=rs1*rs2 unsigned lo
         32'h0012_0???: t_mulh_regs_hw;                        // MULHR RRR rd=high(rs1*rs2) signed
         32'h0013_0???: t_mulhu_regs_hw;                       // MULHUR RRR rd=high(rs1*rs2) unsigned
         32'h0014_0???: t_div_regs_hw;                         // DIVR RRR rd=rs1/rs2 signed
         32'h0015_0???: t_divu_regs_hw;                        // DIVUR RRR rd=rs1/rs2 unsigned
         32'h0016_0???: t_mod_regs_hw;                         // MODR RRR rd=rs1%rs2 signed
         32'h0017_0???: t_modu_regs_hw;                        // MODUR RRR rd=rs1%rs2 unsigned

         //=====================================================================
         // 3-register shift/rotate  (32'h0020_0??? to 32'h0024_0???)
         //=====================================================================
         32'h0020_0???: t_shlr3;                               // SHLR RRR rd=rs1<<rs2[4:0]
         32'h0021_0???: t_shrr3;                               // SHRR RRR rd=rs1>>rs2[4:0] logical
         32'h0022_0???: t_sarr3;                               // SARR RRR rd=rs1>>>rs2[4:0] arithmetic
         32'h0023_0???: t_rotate_left_reg;                     // ROLR RRR rd=rs1 rol rs2[4:0]
         32'h0024_0???: t_rotate_right_reg;                    // RORR RRR rd=rs1 ror rs2[4:0]

         //=====================================================================
         // 3-register compare to register  (32'h0030_0??? to 32'h0039_0???)
         // Result written to rd as 0 or 1
         //=====================================================================
         32'h0030_0???: t_cmpeqr;                              // CMPEQR RRR rd=(rs1==rs2)?1:0
         32'h0031_0???: t_cmpner;                              // CMPNER RRR rd=(rs1!=rs2)?1:0
         32'h0032_0???: t_cmpltr;                              // CMPLTR RRR rd=(rs1<rs2)?1:0 signed
         32'h0033_0???: t_cmpler;                              // CMPLER RRR rd=(rs1<=rs2)?1:0 signed
         32'h0034_0???: t_cmpgtr;                              // CMPGTR RRR rd=(rs1>rs2)?1:0 signed
         32'h0035_0???: t_cmpger;                              // CMPGER RRR rd=(rs1>=rs2)?1:0 signed
         32'h0036_0???: t_cmpultr;                             // CMPULTR RRR rd=(rs1<rs2)?1:0 unsigned
         32'h0037_0???: t_cmpuler;                             // CMPULER RRR rd=(rs1<=rs2)?1:0 unsigned
         32'h0038_0???: t_cmpugtr;                             // CMPUGTR RRR rd=(rs1>rs2)?1:0 unsigned
         32'h0039_0???: t_cmpuger;                             // CMPUGER RRR rd=(rs1>=rs2)?1:0 unsigned

         //=====================================================================
         // 3-register min/max  (32'h0040_0??? to 32'h0043_0???)
         //=====================================================================
         32'h0040_0???: t_min_regs;                            // MINR RRR rd=min(rs1,rs2) signed
         32'h0041_0???: t_max_regs;                            // MAXR RRR rd=max(rs1,rs2) signed
         32'h0042_0???: t_minu_regs;                           // MINUR RRR rd=min(rs1,rs2) unsigned
         32'h0043_0???: t_maxu_regs;                           // MAXUR RRR rd=max(rs1,rs2) unsigned

         //=====================================================================
         // 3-register bit manipulation  (32'h0050_0??? to 32'h0053_0???)
         //=====================================================================
         32'h0050_0???: t_bit_set_reg;                         // BSETRR RRR rd=rs1 with bit rs2 set
         32'h0051_0???: t_bit_clear_reg;                       // BCLRRR RRR rd=rs1 with bit rs2 cleared
         32'h0052_0???: t_bit_toggle_reg;                      // BTGLRR RRR rd=rs1 with bit rs2 toggled
         32'h0053_0???: t_bit_test_reg;                        // BTSTRR RRR rd=(rs1>>rs2[4:0])&1

         //=====================================================================
         // Value-based register ops 0xxx (upper 16 = 0x0000, legacy format)
         //=====================================================================
         32'h0000_01??: t_copy_regs;                           // COPY RR Copy second register into first
         32'h0000_05??: t_cmprr3;                              // CMPRR RR Set flags from first-second (no writeback)
         32'h0000_080?: t_set_reg(w_var1);                     // SETR RV Set register to a value
         32'h0000_081?: t_add_value(w_var1);                   // ADDV RV Add value to register
         32'h0000_082?: t_minus_value(w_var1);                 // MINUSV RV Subtract value from register
         32'h0000_083?: t_compare_reg_value(w_var1);           // CMPRV RV Compare register to value
         32'h0000_084?: t_inc_reg;                             // INCR R Increment register
         32'h0000_085?: t_dec_reg;                             // DECR R Decrement register
         32'h0000_086?: t_and_reg_value(w_var1);               // ANDV RV AND register with value
         32'h0000_087?: t_or_reg_value(w_var1);                // ORV RV OR register with value
         32'h0000_088?: t_xor_reg_value(w_var1);               // XORV RV XOR register with value
         32'h0000_089?: t_set_reg_flags;                       // SETFR R Set register to flags value
         32'h0000_08A?: t_negate_reg;                          // NEGR R 2's complement negate
         32'h0000_08B?: t_abs_reg;                             // ABSR R Absolute value
         32'h0000_08C?: t_sign_extend_byte;                    // SEXTB R Sign extend byte to 32 bits
         32'h0000_08D?: t_left_shift_reg;                      // SHLR1 R Left shift register by 1
         32'h0000_08E?: t_right_shift_reg;                     // SHRR1 R Right shift register by 1 (logical)
         32'h0000_08F?: t_left_shift_a_reg;                    // SHLAR R Left shift arithmetical by 1
         32'h0000_090?: t_right_shift_a_reg;                   // SHRAR R Right shift arithmetical by 1
         32'h0000_091?: t_left_shift_n(w_var1);                // SHLV RV Left shift register by N bits
         32'h0000_092?: t_right_shift_n(w_var1);               // SHRV RV Right shift register by N bits
         32'h0000_093?: t_right_shift_a_n(w_var1);             // SHRAV RV Right shift arithmetical by N
         32'h0000_094?: t_sign_extend_half;                    // SEXTH R Sign extend halfword to 32 bits
         32'h0000_095?: t_zero_extend_byte;                    // ZEXTB R Zero extend byte to 32 bits
         32'h0000_096?: t_zero_extend_half;                    // ZEXTH R Zero extend halfword to 32 bits
         32'h0000_097?: t_byte_swap;                           // BSWAP R Byte swap (endian conversion)
         32'h0000_098?: t_not_reg;                             // NOTR R Bitwise NOT register

         //=====================================================================
         // Bit manipulation by immediate 0A0x-0AFx
         //=====================================================================
         32'h0000_0A0?: t_bit_set_value(w_var1);               // BSET RV Set bit N in register
         32'h0000_0A1?: t_bit_clear_value(w_var1);             // BCLR RV Clear bit N in register
         32'h0000_0A2?: t_bit_toggle_value(w_var1);            // BTGL RV Toggle bit N in register
         32'h0000_0A3?: t_bit_test_value(w_var1);              // BTST RV Test bit N, result in zero flag
         32'h0000_0A8?: t_popcnt;                              // POPCNT R Population count
         32'h0000_0A9?: t_clz;                                 // CLZ R Count leading zeros
         32'h0000_0AA?: t_ctz;                                 // CTZ R Count trailing zeros
         32'h0000_0AB?: t_bit_reverse;                         // BITREV R Reverse all bits
         32'h0000_0AC?: t_extract_bits(w_var1);                // BEXTR RV Extract bit field
         32'h0000_0AD?: t_deposit_bits(w_var1);                // BDEP RV Deposit bit field

         //=====================================================================
         // Hardware multiply/divide by value 0Bxx
         //=====================================================================
         32'h0000_0B8?: t_mul_value_hw(w_var1);                // MULV RV Multiply register by value (signed)
         32'h0000_0B9?: t_div_value_hw(w_var1);                // DIVV RV Divide register by value (signed)
         32'h0000_0BA?: t_mod_value_hw(w_var1);                // MODV RV Modulo register by value (signed)

         //=====================================================================
         // Indexed memory access 0C0x-0EFx
         //=====================================================================
         32'h0000_0C??: t_load_indexed(w_var1);                // LDIDX RRV first=mem[second+var1]
         32'h0000_0D??: t_store_indexed(w_var1);               // STIDX RRV mem[second+var1]=first
         32'h0000_0E??: t_load_indexed_reg(w_var1);            // LDIDXR RRV first=mem[second+reg[var1]]

         //=====================================================================
         // Rotate by immediate 0F8x-0FDx
         //=====================================================================
         32'h0000_0F8?: t_rotate_left;                         // ROLR1 R Rotate left by 1
         32'h0000_0F9?: t_rotate_right;                        // RORR1 R Rotate right by 1
         32'h0000_0FA?: t_rotate_left_carry;                   // ROLCR R Rotate left through carry
         32'h0000_0FB?: t_rotate_right_carry;                  // RORCR R Rotate right through carry
         32'h0000_0FC?: t_rotate_left_n(w_var1);               // ROLV RV Rotate left by N bits
         32'h0000_0FD?: t_rotate_right_n(w_var1);              // RORV RV Rotate right by N bits

         //=====================================================================
         // Flow control 1xxx
         //=====================================================================
         32'h0000_1000: t_cond_jump(w_var1, 1'b1);             // JMP V Jump
         32'h0000_1001: t_cond_jump(w_var1, r_zero_flag);      // JMPZ V Jump if zero
         32'h0000_1002: t_cond_jump(w_var1, !r_zero_flag);     // JMPNZ V Jump if not zero
         32'h0000_1003: t_cond_jump(w_var1, r_equal_flag);     // JMPE V Jump if equal
         32'h0000_1004: t_cond_jump(w_var1, !r_equal_flag);    // JMPNE V Jump if not equal
         32'h0000_1005: t_cond_jump(w_var1, r_carry_flag);     // JMPC V Jump if carry
         32'h0000_1006: t_cond_jump(w_var1, !r_carry_flag);    // JMPNC V Jump if not carry
         32'h0000_1007: t_cond_jump(w_var1, r_overflow_flag);  // JMPO V Jump if overflow
         32'h0000_1008: t_cond_jump(w_var1, !r_overflow_flag); // JMPNO V Jump if not overflow
         32'h0000_1009: t_cond_call(w_var1, 1'b1);             // CALL V Call function
         32'h0000_100A: t_cond_call(w_var1, r_zero_flag);      // CALLZ V Call if zero
         32'h0000_100B: t_cond_call(w_var1, !r_zero_flag);     // CALLNZ V Call if not zero
         32'h0000_100C: t_cond_call(w_var1, r_equal_flag);     // CALLE V Call if equal
         32'h0000_100D: t_cond_call(w_var1, !r_equal_flag);    // CALLNE V Call if not equal
         32'h0000_100E: t_cond_call(w_var1, r_carry_flag);     // CALLC V Call if carry
         32'h0000_100F: t_cond_call(w_var1, !r_carry_flag);    // CALLNC V Call if not carry
         32'h0000_1010: t_cond_call(w_var1, r_overflow_flag);  // CALLO V Call if overflow
         32'h0000_1011: t_cond_call(w_var1, !r_overflow_flag); // CALLNO V Call if not overflow
         32'h0000_1012: t_ret;                                  // RET Return from call
         32'h0000_1013: t_cond_jump(w_var1, r_sign_flag);      // JMPS V Jump if sign (negative)
         32'h0000_1014: t_cond_jump(w_var1, !r_sign_flag);     // JMPNS V Jump if not sign
         32'h0000_1015: t_cond_jump(w_var1, r_less_flag);      // JMPLT V Jump if less-than (signed)
         32'h0000_1016: t_cond_jump(w_var1, r_less_flag | r_equal_flag);   // JMPLE V Jump if less-or-equal
         32'h0000_1017: t_cond_jump(w_var1, !r_less_flag & !r_equal_flag); // JMPGT V Jump if greater-than
         32'h0000_1018: t_cond_jump(w_var1, !r_less_flag);                      // JMPGE V Jump if greater-or-equal (signed)
         32'h0000_1019: t_cond_jump(w_var1, r_ult_flag);                         // JMPULT V Jump if unsigned less-than
         32'h0000_101A: t_cond_jump(w_var1, r_ult_flag | r_equal_flag);          // JMPULE V Jump if unsigned less-or-equal
         32'h0000_101B: t_cond_jump(w_var1, !r_ult_flag & !r_equal_flag);        // JMPUGT V Jump if unsigned greater-than
         32'h0000_101C: t_cond_jump(w_var1, !r_ult_flag);                        // JMPUGE V Jump if unsigned greater-or-equal
         32'h0000_102?: t_jump_reg;                             // JMPR R Jump to address in register

         //=====================================================================
         // SPI LCD Control 2xxx
         //=====================================================================
         32'h0000_200?: spi_dc_write_command_reg;              // CDCDMR R LCD command with register
         32'h0000_201?: spi_dc_data_command_reg;               // LCDDATAR R LCD data with register
         32'h0000_2021: spi_dc_write_command_value(w_var1);    // LCDCMDV V LCD write command value
         32'h0000_2022: spi_dc_write_data_value(w_var1);       // LCDDATAV V LCD data with value
         32'h0000_2023: t_lcd_reset_value(w_var1);             // LCD V Reset line

         //=====================================================================
         // Board LED and Switch 3xxx
         //=====================================================================
         32'h0000_300?: t_led_reg;                             // LEDR R Set LEDs with register
         32'h0000_301?: t_get_switch_reg;                      // SWR R Get switch status into register
         32'h0000_302?: t_7_seg1_reg;                          // 7SEG1R R Set 7 Seg 1 to register
         32'h0000_303?: t_7_seg2_reg;                          // 7SEG2R R Set 7 Seg 2 to register
         32'h0000_304?: t_7_seg_reg;                           // 7SEGR R Set 7 Seg to register
         32'h0000_305?: t_led_rgb1_reg;                        // RGB1R R RGB 1 from register
         32'h0000_306?: t_led_rgb2_reg;                        // RGB2R R RGB 2 from register
         32'h0000_3070: t_led_value(w_var1);                   // LEDV V Set LED to value
         32'h0000_3071: t_7_seg1_value(w_var1);                // 7SEG1V V Set 7 Seg 1 to value
         32'h0000_3072: t_7_seg2_value(w_var1);                // 7SEG2V V Set 7 Seg 2 to value
         32'h0000_3073: t_7_seg_blank;                         // 7SEGBLANK Blank 7 Seg
         32'h0000_3074: t_led_rgb1_value(w_var1);              // RGB1V V RGB 1 from value
         32'h0000_3075: t_led_rgb2_value(w_var1);              // RGB2V V RGB 2 from value

         //=====================================================================
         // Stack control 4xxx
         //=====================================================================
         32'h0000_400?: t_stack_push_reg;                      // PUSH R Push register onto stack
         32'h0000_401?: t_stack_pop_reg;                       // POP R Pop stack into register
         32'h0000_4020: t_stack_push_value(w_var1);            // PUSHV V Push 32-bit value onto stack
         32'h0000_403?: t_get_sp;                              // GETSP R Copy SP into register
         32'h0000_404?: t_set_sp;                              // SETSP R Set SP from register
         32'h0000_4050: t_add_sp(w_var1);                      // ADDSP V Add signed immediate to SP
         32'h0000_406?: t_call_reg;                            // CALLR R Call to address in register

         //=====================================================================
         // Communication 5xxx
         //=====================================================================
         32'h0000_5000: t_test_message;                        // TESTMSG Send test UART message
         32'h0000_5001: t_tx_newline;                          // NEWLINE Send UART newline
         32'h0000_5002: t_tx_value_of_mem(w_var1);             // TXMEM V Send 8 bytes value of memory location
         32'h0000_5003: t_tx_string_at_mem(w_var1);            // TXSTRMEM V Send string at memory
         32'h0000_501?: t_tx_reg;                              // TXR R Send 8 bytes reg value in message
         32'h0000_502?: t_tx_value_of_mem_at_reg;              // TXMEMR R Send value at memory of register
         32'h0000_503?: t_tx_char_from_reg_value;              // TXCHARMEMR R Send char at memory from register
         32'h0000_504?: t_tx_string_at_reg;                    // TXSTRMEMR R Send string at memory from register
         32'h0000_505?: t_rx_blocking;                        // RXRB R Blocking receive byte into register
         32'h0000_506?: t_rx_nonblocking;                     // RXRNB R Non-blocking receive byte into register (zero_flag=1 if empty)

         //=====================================================================
         // CPU Setting 6xxx
         //=====================================================================
         32'h0000_60??: t_set_interrupt_regs;                  // INTSETRR RR Set interrupt from registers

         //=====================================================================
         // Memory actions 7xxx
         //=====================================================================
         32'h0000_70??: t_set_mem_from_reg_reg;                // MEMSETRR RR mem[second]=first
         32'h0000_71??: t_set_reg_from_mem_reg;                // MEMREADRR RR first=mem[second]
         32'h0000_720?: t_set_mem_from_value_reg(w_var1);      // MEMSETR RV Set memory from value
         32'h0000_721?: t_set_reg_from_mem_value(w_var1);      // MEMREADR RV Set register from memory at value
         32'h0000_73??: t_store_indexed_reg(w_var1);           // STIDXR RRV mem[second+reg[var1]]=first

         //=====================================================================
         // Byte memory access 74xx-75xx
         //=====================================================================
         32'h0000_74??: t_memset8;                              // MEMSET8 RR mem8[reg2]=reg1[7:0] (byte addr)
         32'h0000_75??: t_memget8;                              // MEMGET8 RR reg1=zero_ext(mem8[reg2]) (byte addr)

         //=====================================================================
         // 64-bit memory access 76xx-79xx
         //=====================================================================
         32'h0000_76??: t_memset64;                             // MEMSET64 RR mem64[reg2]=reg1 (64-bit word store)
         32'h0000_77??: t_memget64;                             // MEMGET64 RR reg1=mem64[reg2] (64-bit word load)
         32'h0000_78??: t_store_indexed_64(w_var1);             // STIDX64 RRV mem64[reg2+var1]=reg1 (64-bit indexed store)
         32'h0000_79??: t_load_indexed_64(w_var1);              // LDIDX64 RRV reg1=mem64[reg2+var1] (64-bit indexed load)

         //=====================================================================
         // Other Fxxx
         //=====================================================================
         32'h0000_F00?: t_delay_reg;                           // DELAYR R Delay by register
         32'h0000_F010: t_nop;                                 // NOP No operation
         32'h0000_F011: t_halt;                                // HALT Freeze and hang
         32'h0000_F012: t_reset;                               // RESET Reset
         32'h0000_F013: t_delay(w_var1);                       // DELAYV V Delay by value

         default: begin
            r_SM <= HCF_1;  // Halt and catch fire error 1
            r_error_code <= ERR_INV_OPCODE;
         end
      endcase
   end
endtask
