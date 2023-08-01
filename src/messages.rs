use chrono::{Local, NaiveTime};
use colored::{ColoredString, Colorize};

#[derive(PartialEq, Eq, Debug)]
/// Enum for message type
pub enum MessageType {
    /// Error message
    Error,
    /// Warning message
    Warning,
    /// Information message
    Information,
}

#[derive(Debug)]
/// Struct for message
pub struct Message {
    /// Text of message
    pub text: String,
    /// File name of file causing message if exists
    pub file_name: Option<String>,
    /// Line number in file causing message if exists
    pub line_number: Option<u32>,
    /// Message type
    pub level: MessageType,
    /// Time of message
    pub time: Option<NaiveTime>,
}

#[cfg(not(tarpaulin_include))]
#[allow(clippy::missing_docs_in_private_items)]
impl Default for &Message {
    fn default() -> &'static Message {
        static VALUE: Message = Message {
            text: String::new(),
            file_name: None,
            line_number: None,
            level: MessageType::Information,
            time: None,
        };
        &VALUE
    }
}

#[derive(Debug)]
/// Struct for list of messages
pub struct MsgList {
    /// Vector of messages
    pub list: Vec<Message>,
}

/// Implementation of `MsgList`
impl MsgList {
    /// Create new `MsgList`
    pub const fn new() -> Self {
        Self { list: Vec::new() }
    }

    /// Push message to `MsgList`
    pub fn push(
        &mut self,
        name: String,
        line_number: Option<u32>,
        file_name: Option<String>,
        msg_type: MessageType,
    ) {
        self.list.push(Message {
            text: name,
            line_number,
            file_name,
            level: msg_type,
            time: Some(Local::now().time()),
        });
    }

    /// Returns number of warnings in `MsgList`
    pub fn number_by_type(&self, msg_type: &MessageType) -> usize {
        let warnings = self.list.iter().filter(|x| x.level == *msg_type).count();
        warnings
    }
}

/// Print out all messages
///
/// Prints all the message in passed `MsgList` vector to terminal with coloured messages
#[allow(clippy::module_name_repetitions)]
#[allow(clippy::print_stdout)]
#[cfg(not(tarpaulin_include))] // Cannot test this function as it prints to terminal
pub fn print_messages(msg_list: &MsgList) {
    for msg in &msg_list.list {
        let message_level: ColoredString = match msg.level {
            MessageType::Information => "I".to_owned().green(),
            MessageType::Warning => "W".to_owned().yellow(),
            MessageType::Error => "E".to_owned().red(),
        };
        println!(
            "{}",
            if msg.line_number.is_some() {
                if msg.file_name.is_some() {
                    format!(
                        "{} {} Line {} in file {}. {} ",
                        msg.time.unwrap_or_default().format("%H:%M:%S%.3f"),
                        message_level,
                        msg.line_number.unwrap_or_default(),
                        msg.file_name.clone().unwrap_or_default(),
                        msg.text
                    )
                } else {
                    format!(
                        "{} {} Line {}. {} ",
                        msg.time.unwrap_or_default().format("%H:%M:%S%.3f"),
                        message_level,
                        msg.line_number.unwrap_or_default(),
                        msg.text
                    )
                }
            } else if msg.file_name.is_some() {
                format!(
                    "{} {} In file {}. {} ",
                    msg.time.unwrap_or_default().format("%H:%M:%S%.3f"),
                    message_level,
                    msg.file_name.clone().unwrap_or_default(),
                    msg.text
                )
            } else {
                format!(
                    "{} {} {} ",
                    msg.time.unwrap_or_default().format("%H:%M:%S%.3f"),
                    message_level,
                    msg.text
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
            "Test".to_owned(),
            None,
            Some("test".to_owned()),
            MessageType::Information,
        );
        assert_eq!(msg_list.list.len(), 1);
        assert_eq!(msg_list.list.get(0).unwrap_or_default().text, "Test");
        assert_eq!(msg_list.list.get(0).unwrap_or_default().text, "Test");
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().level,
            MessageType::Information
        );
        assert_eq!(msg_list.list.get(0).unwrap_or_default().line_number, None);
    }

    #[test]
    // Test that the number of errors is correct
    fn test_number_errors() {
        let mut msg_list = MsgList::new();
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 0);
        msg_list.push("Test".to_owned(), None, None, MessageType::Information);
        msg_list.push("Test".to_owned(), None, None, MessageType::Warning);
        msg_list.push("Test".to_owned(), None, None, MessageType::Error);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 1);
    }

    #[test]
    // Test number of warnings
    fn test_number_warnings() {
        let mut msg_list = MsgList::new();
        assert_eq!(msg_list.number_by_type(&MessageType::Warning), 0);
        msg_list.push("Test".to_owned(), None, None, MessageType::Information);
        msg_list.push("Test".to_owned(), None, None, MessageType::Warning);
        msg_list.push("Test".to_owned(), None, None, MessageType::Error);
        assert_eq!(msg_list.number_by_type(&MessageType::Warning), 1);
    }
}
