use chrono::{Local, NaiveTime};
use colored::{ColoredString, Colorize};

#[derive(PartialEq, Eq, Debug)]
pub enum MessageType {
    Error,
    Warning,
    Information,
}

#[derive(Debug)]
pub struct Message {
    pub name: String,
    pub file_name: Option<String>,
    pub line_number: Option<u32>,
    pub level: MessageType,
    pub time: NaiveTime,
}

#[derive(Debug)]
pub struct MsgList {
    pub list: Vec<Message>,
}

impl MsgList {
    pub const fn new() -> Self {
        Self { list: Vec::new() }
    }

    pub fn push(
        &mut self,
        name: String,
        line_number: Option<u32>,
        file_name: Option<String>,
        msg_type: MessageType,
    ) {
        self.list.push(Message {
            name,
            line_number,
            file_name,
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
#[allow(clippy::print_stdout)]
#[cfg(not(tarpaulin_include))] // Cannot test this function as it prints to terminal
pub fn print_messages(msg_list: &mut MsgList) {
    for msg in &msg_list.list {
        let message_level: ColoredString = match msg.level {
            MessageType::Information => "I".to_string().green(),
            MessageType::Warning => "W".to_string().yellow(),
            MessageType::Error => "E".to_string().red(),
        };
        println!(
            "{}",
            if msg.line_number.is_some() {
                if msg.file_name.is_some() {
                    format!(
                        "{} {} Line {} in file {}. {} ",
                        msg.time.format("%H:%M:%S%.3f"),
                        message_level,
                        msg.line_number.unwrap_or_default(),
                        msg.file_name.clone().unwrap_or_default(),
                        msg.name
                    )
                }
                else {
                    format!(
                        "{} {} Line {}. {} ",
                        msg.time.format("%H:%M:%S%.3f"),
                        message_level,
                        msg.line_number.unwrap_or_default(),
                        msg.name
                    )
                }
            } else if msg.file_name.is_some() {
                format!(
                    "{} {} In file {}. {} ",
                    msg.time.format("%H:%M:%S%.3f"),
                    message_level,
                    msg.file_name.clone().unwrap_or_default(),
                    msg.name
                )
            }
            else {
                format!(
                    "{} {} {} ",
                    msg.time.format("%H:%M:%S%.3f"),
                    message_level,
                    msg.name
                )
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Test that the message list is created correctly
    fn test_msg_list() {
        let mut msg_list = MsgList::new();
        msg_list.push(
            "Test".to_string(),
            None,
            Some("test".to_string()),
            MessageType::Information,
        );
        assert_eq!(msg_list.list.len(), 1);
        assert_eq!(msg_list.list[0].name, "Test");
        assert_eq!(msg_list.list[0].level, MessageType::Information);
        assert_eq!(msg_list.list[0].line_number, None);
    }

    #[test]
    // Test that the number of errors is correct
    fn test_number_errors() {
        let mut msg_list = MsgList::new();
        msg_list.push("Test".to_string(), None, None, MessageType::Information);
        msg_list.push("Test".to_string(), None, None, MessageType::Warning);
        msg_list.push("Test".to_string(), None, None, MessageType::Error);
        assert_eq!(msg_list.number_errors(), 1);
    }

    #[test]
    // Test number of warnings
    fn test_number_warnings() {
        let mut msg_list = MsgList::new();
        msg_list.push("Test".to_string(), None, None, MessageType::Information);
        msg_list.push("Test".to_string(), None, None, MessageType::Warning);
        msg_list.push("Test".to_string(), None, None, MessageType::Error);
        assert_eq!(msg_list.number_warnings(), 1);
    }
}
