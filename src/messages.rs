use chrono::{Local, NaiveTime};
use colored::{ColoredString, Colorize};

#[derive(PartialEq)]
pub enum MessageType {
    Error,
    Warning,
    Info,
}

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
            name,
            line_number,
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

/// Print out all messages
/// 
/// Prints all the message in passed `MsgList` vector to terminal with coloured messages
#[allow(clippy::module_name_repetitions)]
pub fn print_messages(msg_list: &mut MsgList) {
    for msg in &msg_list.list {
        
        let warning: ColoredString = match msg.level {
            MessageType::Info => "I".to_string().green(),
            MessageType::Warning => "W".to_string().yellow(),
            MessageType::Error => "E".to_string().red(),
        };
            println!("{}",if msg.line_number.is_some() {
            format!(
                "{} {} Line {}. {} ",
                msg.time.format("%H:%M:%S%.3f"),
                warning,
                msg.line_number.unwrap(),
                msg.name
            )
        } else {
            format!("{} {} {} ", msg.time.format("%H:%M:%S.%3f"), warning, msg.name)
        });
    }
}
