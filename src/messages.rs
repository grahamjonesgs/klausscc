use chrono::{NaiveTime,Local};

#[derive(Debug, PartialEq, Clone)]
pub enum MessageType {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub name: String,
    pub line_number: Option<u32>,
    pub level: MessageType,
    pub time: NaiveTime,
}

pub fn add_message(name: String,  line_number: Option<u32>, msg_type: MessageType, msgs: &mut Vec<Message>) {
    let new_message = Message {
        name: String::from(name),
        line_number: line_number,
        level: msg_type,
        time: Local::now().time(),
    };
    msgs.push(new_message);
}

pub fn number_errors(msgs: &mut Vec<Message>) -> usize {
    let errors = msgs
        .iter()
        .filter(|&x| x.level == MessageType::Error)
        .count();
    errors
}

pub fn number_warnings(msgs: &mut Vec<Message>) -> usize {
    let errors = msgs
        .iter()
        .filter(|&x| x.level == MessageType::Warning)
        .count();
    errors
}

pub fn print_messages(msgs: &mut Vec<Message>) {
    for msg in msgs.clone() {
        let message : String;
        let warning: String;
        match msg.level {
            MessageType::Info => warning = "I".to_string(),
            MessageType::Warning => warning = "W".to_string(),
            MessageType::Error => warning = "E".to_string(),
        };
        if msg.line_number.is_some() {
        message=format!("{} {} Line {} {} ",msg.time.to_string(),warning,msg.line_number.unwrap(),msg.name);
        }
        else {
            message=format!("{} {} {} ",msg.time.to_string(),warning,msg.name);
        }
        //message = &msg.time.to_string() + " " + &warning.to_string() + ". Line " + &msg.line_number.to_string() + " " + &msg.name;
        println!("{}", message);
    }
}
