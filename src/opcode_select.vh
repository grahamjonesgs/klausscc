// Can be read by assembler, so format is fixed. Opcode must be first word in comment. If opcode takes variable, it mst be passed as w_var1
/* Macro definition

$LEVEL1 $LEVEL2 %1 / $LEVEL3 %2
$LEVEL2 POP %1
$LEVEL3 PUSH %1
$VARS POP %1 / POP %2 / POP %2 / POP %1 / POP %9
// Expected level1 pop %1 / push %5
*/

task t_opcode_select;
    begin
        casez(w_opcode[15:0])


            // Register control 0xxx
            16'h01??: t_copy_regs;                              // COPY Copy register
            16'h02??: t_and_regs;                               // AND AND register
            16'h03??: t_or_regs;                                // OR OR register
            16'h04??: t_xor_regs;                               // XOR XOR register
            16'h05??: t_compare_regs;                           // CMPRR Compare registers
            16'h06??: t_add_regs;                               // ADDRR Compare registers
            16'h07??: t_minus_regs;                             // MINUSRR Compare registers
            16'h080?: t_set_reg(w_var1);                        // SETR Set register value
            16'h081?: t_add_value(w_var1);                      // ADDV Increament register by value
            16'h082?: t_minus_value(w_var1);                    // MINUSV Decreament register by value
            16'h083?: t_compare_reg_value(w_var1);              // CMPRV Compare register to value
            16'h084?: t_inc_reg;                                // INCR Increament register
            16'h085?: t_dec_reg;                                // DECR Decreament register
            16'h086?: t_and_reg_value(w_var1);                  // ANDV AND register with value
            16'h087?: t_or_reg_value(w_var1);                   // ORV OR register with value
            16'h088?: t_xor_reg_value(w_var1);                  // XORV XOR register with value
            16'h089?: t_set_reg_flags;                          // SETFR Set register to flags value
            16'h08A?: t_negate_reg;                             // NEGR Set register 2's comp
            //     16'h08B?: t_set_reg_from_memory(w_var1);            // SETRM Set register to memory
            //     16'h08C?: t_set_memory_from_reg(w_var1);            // SETMR Set register to memory
            16'h08D?: t_left_shift_reg;                         // SHLR Left shift reg
            16'h08E?: t_right_shift_reg;                        // SHRR Right shift reg
            16'h08F?: t_left_shift_a_reg;                       // SHLAR Left shift arithmetical reg
            16'h090?: t_right_shift_a_reg;                      // SHRAR Right shift arithmetical reg


            // Flow control 1xxx
            16'h1000: t_cond_jump(w_var1,1'b1);                 // JMP Jump
            16'h1001: t_cond_jump(w_var1,r_zero_flag);          // JMPZ Jump if zero
            16'h1002: t_cond_jump(w_var1,!r_zero_flag);         // JMPNZ Jump if not zero
            16'h1003: t_cond_jump(w_var1,r_equal_flag);         // JMPE Jump if equal
            16'h1004: t_cond_jump(w_var1,!r_equal_flag);        // JMPNE Jump if not equal
            16'h1005: t_cond_jump(w_var1,r_carry_flag);         // JMPC Jump if carry
            16'h1006: t_cond_jump(w_var1,!r_carry_flag);        // JMPNC Jump if not carry
            16'h1007: t_cond_jump(w_var1,r_overflow_flag);      // JMPO Jump if overflow
            16'h1008: t_cond_jump(w_var1,!r_overflow_flag);     // JMPNO Jump if not overflow
            16'h1009: t_cond_call(w_var1,1'b1);                 // CALL Call function
            16'h100A: t_cond_call(w_var1,r_zero_flag);          // CALLZ Call if zero
            16'h100B: t_cond_call(w_var1,!r_zero_flag);         // CALLNZ Call if not zero
            16'h100C: t_cond_call(w_var1,r_equal_flag);         // CALLE Call if equal
            16'h100D: t_cond_call(w_var1,!r_equal_flag);        // CALLNE Call if not equal
            16'h100E: t_cond_call(w_var1,r_carry_flag);         // CALLC Call if carry
            16'h100F: t_cond_call(w_var1,!r_carry_flag);        // CALLNC Call if not carry
            16'h1010: t_cond_call(w_var1,r_overflow_flag);      // CALLO Call if overflow
            16'h1011: t_cond_call(w_var1,!r_overflow_flag);     // CALLNO Call if not overflow
            16'h1012: t_ret;                                    // RET Return from call

            // SPI LCD Control 2xxx
            16'h200?: spi_dc_write_command_reg;                 // CDCDMR LCD command with register
            16'h201?: spi_dc_data_command_reg;                  // LCDDATAR LCD data with register
            16'h2021: spi_dc_write_command_value(w_var1);       // LCDCMDV
            16'h2022: spi_dc_write_data_value(w_var1);          // LCDDATAV LCD data with value
            16'h2023: t_lcd_reset_value(w_var1);                // LCD Reset line


            // Board LED and Switch 3xxx
            16'h300?: t_led_reg;                                // LEDR set with register
            16'h301?: t_get_switch_reg;                         // SWR Get switch status into register
            16'h302?: t_7_seg1_reg;                             // 7SEG1R Set 7 Seg 1 to register
            16'h303?: t_7_seg2_reg;                             // 7SEG2R Set 7 Seg 2 to register
            16'h304?: t_7_seg_reg;                              // 7SEGR Set 7 Seg to register
            16'h305?: t_led_rgb1_reg;                           // RGB1R RGB 1 from register
            16'h306?: t_led_rgb2_reg;                           // RGB2R RGB 2 from register
            16'h3070: t_led_value(w_var1);                      // LEDV Set LED value
            16'h3071: t_7_seg1_value(w_var1);                   // 7SEG1V Set 7 Seg 1 to value
            16'h3072: t_7_seg2_value(w_var1);                   // 7SEG2V Set 7 Seg 2 to value
            16'h3073: t_7_seg_blank;                            // 7SEGBLANK Blank 7 Seg
            16'h3074: t_led_rgb1_value(w_var1);                 // RGB1V RGB 1 from value
            16'h3075: t_led_rgb2_value(w_var1);                 // RGB2V RGB 2 from value

            // Stack control 4xxx
            16'h400?: t_stack_push_reg;                         // PUSH Push register onto stack
            16'h401?: t_stack_pop_reg;                          // POP Pop stack into register
            16'h4020: t_stack_push_value(w_var1);               // PUSHV Push value onto stack

            // Coms 5xxxx
            16'h5000: t_test_message;                           // TESTMSG test UART message

            // CPU Setting 6xxxx

            16'h60??: t_set_interupt_regs;                      // INTSETRR Set interupt from registers

            // Memory actions 7xxx
            16'h70??: t_set_mem_from_reg_reg;                   // MEMSETRR Set mem location given in register to contents of register (first in order is value, second is location)
            16'h71??: t_set_reg_from_mem_reg;                   // MEMREADRR Set contents of register to location given in register (first in order is reg to be set, second is location)

            16'h720?: t_set_mem_from_value_reg(w_var1);         // MEMSETR Set mem location given in value to contents of register
            16'h721?: t_set_reg_from_mem_value(w_var1);         // MEMREADR Set contents of register to location given in value

            // Other Fxxx
            16'hF00?: t_delay_reg;                              // DELAYR Delay with register
            16'hF010: t_nop;                                    // NOP No opperation
            16'hF011: t_halt;                                   // HALT Freeze and hang
            16'hF012: t_reset;                                  // RESET Reset
            16'hF013: t_delay(w_var1);                          // DELAYV Set value

            default:
            begin
                r_SM<=HCF_1; // Halt and catch fire error 1
                r_error_code<=ERR_INV_OPCODE;
            end // default case
        endcase //casez(w_opcode[15:0])
    end
endtask

