#[derive(Debug)]
#[derive(PartialEq)]
#[derive(Clone)]
pub enum MessageType {
    Error,
    Warning,
    Info
}

#[derive(Debug)]
#[derive(Clone)]
pub struct Message {
    pub name: String,
    pub line_number: u32,
    pub level: MessageType,
}

pub fn add_message(name: &str, msg_type: MessageType, line_number: u32, msgs: &mut Vec<Message>) {
    let new_message = Message {
        name: String::from(name),
        line_number: line_number,
        level: msg_type,
    };
    msgs.push(new_message);
}

pub fn number_errors(msgs: &mut Vec<Message>) -> usize {
    let errors  = msgs.
                    iter().
                    filter(|&x| x.level == MessageType::Error).
                    count();
    errors
}

pub fn number_warnings(msgs: &mut Vec<Message>) -> usize {
    let errors  = msgs.
                    iter().
                    filter(|&x| x.level == MessageType::Warning).
                    count();
    errors
}

pub fn print_messages(msgs: &mut Vec<Message>) {

for msg in msgs.clone() {
    let mut message;
    match msg.level {
        MessageType::Info => message = "I".to_string(),
        MessageType::Warning => message = "W".to_string(),
        MessageType::Error => message = "E".to_string(),
    };
    message=message+". Line " +&msg.line_number.to_string() + " " + &msg.name;
    println!("{}",message);
    }
    
}
