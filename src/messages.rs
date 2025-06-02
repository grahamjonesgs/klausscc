use chrono::{Local, NaiveTime};
use colored::{ColoredString, Colorize as _};

#[derive(Debug)]
/// Struct for message
pub struct Message {
    /// File name of file causing message if exists
    pub file_name: Option<String>,
    /// Message type
    pub level: MessageType,
    /// Line number in file causing message if exists
    pub line_number: Option<u32>,
    /// Text of message
    pub text: String,
    /// Time of message
    pub time: Option<NaiveTime>,
}
#[derive(PartialEq, Eq, Debug)]
/// Enum for message type
pub enum MessageType {
    /// Error message
    Error,
    /// Information message
    Information,
    /// Warning message
    Warning,
}

#[cfg(not(tarpaulin_include))]
#[allow(clippy::missing_docs_in_private_items, reason = "Default implementation for reference to Message is only used internally for tests")]
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

#[derive(Debug, Default)]
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

   

    /// Returns number of warnings in `MsgList`
    pub fn number_by_type(&self, msg_type: &MessageType) -> usize {
        let warnings = self.list.iter().filter(|x| x.level == *msg_type).count();
        warnings
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
}

/// Print out all messages
///
/// Prints all the message in passed `MsgList` vector to terminal with coloured messages
#[allow(clippy::module_name_repetitions, reason = "Function name matches module name for clarity in user-facing API")]
#[allow(clippy::print_stdout, reason = "Printing to stdout is intended for user-facing message output")]
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
        assert_eq!(msg_list.list.first().unwrap_or_default().text, "Test");
        assert_eq!(msg_list.list.first().unwrap_or_default().text, "Test");
        assert_eq!(
            msg_list.list.first().unwrap_or_default().level,
            MessageType::Information
        );
        assert_eq!(msg_list.list.first().unwrap_or_default().line_number, None);
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
