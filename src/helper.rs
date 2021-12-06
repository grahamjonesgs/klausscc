// Check if end of first word is colon
pub fn is_label (line: &String) -> bool {
    let words=line.split_whitespace();
    for (i,word)  in words.enumerate() {
        //println!("Word {} is {}",i,word);
        if i==0 && word.ends_with(":") {return true}
    }
    false
}

