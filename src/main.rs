
use clap::{Arg, App};
mod files;
use files::parse_opcodes;


fn main() {
    let matches = App::new("Klauss Assembler")
        .version("0.0.1")
        .author("Graham Jones")
        .about("Assembler for FPGA_CPU")
        .arg(Arg::with_name("file")
                 .short("f")
                 .long("file")
                 .takes_value(true)
                 .help("Opcode source file"))
        .arg(Arg::with_name("num")
                 .short("n")
                 .long("number")
                 .takes_value(true)
                 .help("Dummy number"))
        .get_matches();

    let myfile = matches.value_of("file").unwrap_or("input.txt");
    println!("The file passed is: {}", myfile);

   

    let oplist = parse_opcodes("/Users/graham/Documents/src/rust/opttest/src/opcode_select.vh");
    println!("Finished {:?}",oplist[10].comment);
    //println!("Finished {:?}",oplist);
}