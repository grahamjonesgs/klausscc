
use clap::{Arg, App};
mod files;
use files::lines_from_file;

fn main() {
    let matches = App::new("My Test Program")
        .version("0.1.0")
        .author("Hackerman Jones <hckrmnjones@hack.gov>")
        .about("Teaches argument parsing")
        .arg(Arg::with_name("file")
                 .short("f")
                 .long("file")
                 .takes_value(true)
                 .help("A cool file"))
        .arg(Arg::with_name("num")
                 .short("n")
                 .long("number")
                 .takes_value(true)
                 .help("Five less than your favorite number"))
        .get_matches();

    let myfile = matches.value_of("file").unwrap_or("input.txt");
    println!("The file passed is: {}", myfile);

    let num_str = matches.value_of("num");
    match num_str {
        None => println!("No idea what your favorite number is."),
        Some(s) => {
            match s.trim().parse::<i32>() {
                Ok(n) => println!("Your favorite number must be {}.", n + 5),
                Err(_) => println!("That's not a number!{}!", s),
            }
        }
    }
    let lines = lines_from_file("/etc/hosts");
    for line in &lines {
        println!("{:?}", line);
    }
    let mut new_lines = Vec::new();// :  Vec<&str> ;
    println!("Size of file {}",lines.len());

    for line in &lines {
        let words=line.split_whitespace();
        for word in words {
            println!("Word is {}",word);
            new_lines.push(word);
        }
    }
}