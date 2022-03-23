use chrono::{Local, NaiveTime};

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

pub struct MsgList {
    list: Vec<Message>,
}

impl MsgList {
    pub fn new() -> MsgList {
        MsgList { list: Vec::new() }
    }
    pub fn push(&mut self, name: String, line_number: Option<u32>, msg_type: MessageType) {
        let _ = &mut self.list.push(Message {
            name: name,
            line_number: line_number,
            level: msg_type,
            time: Local::now().time(),
        });
    }
    pub fn number_errors(&self) -> usize {
        let errors = self
            .list
            .iter()
            .filter(|&x| x.level == MessageType::Error)
            .count();
        errors
    }
    pub fn number_warnings(&self) -> usize {
        let errors = self
            .list
            .iter()
            .filter(|&x| x.level == MessageType::Warning)
            .count();
        errors
    }
}

pub fn print_messages(msg_list: &mut MsgList) {
    for msg in &msg_list.list {
        let message: String;
        let warning: String;
        match msg.level {
            MessageType::Info => warning = "I".to_string(),
            MessageType::Warning => warning = "W".to_string(),
            MessageType::Error => warning = "E".to_string(),
        };
        if msg.line_number.is_some() {
            message = format!(
                "{} {} Line {} {} ",
                msg.time.to_string(),
                warning,
                msg.line_number.unwrap(),
                msg.name
            );
        } else {
            message = format!("{} {} {} ", msg.time.to_string(), warning, msg.name);
        }
        //message = &msg.time.to_string() + " " + &warning.to_string() + ". Line " + &msg.line_number.to_string() + " " + &msg.name;
        println!("{}", message);
    }
}
