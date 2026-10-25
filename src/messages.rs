use chrono::{Local, NaiveTime};
use colored::{ColoredString, Colorize as _};

#[derive(Debug)]
/// Struct for message.
pub struct Message {
    /// File name of file causing message if exists.
    pub file_name: Option<String>,
    /// Message type.
    pub level: MessageType,
    /// Line number in file causing message if exists.
    pub line_number: Option<u32>,
    /// Text of message.
    pub text: String,
    /// Time of message.
    pub time: Option<NaiveTime>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Enum for message type.
pub enum MessageType {
    /// Error message.
    Error,
    /// Information message.
    Information,
    /// Warning message.
    Warning,
}

#[derive(Debug, Default)]
/// Struct for list of messages.
pub struct MsgList {
    /// Vector of messages.
    pub list: Vec<Message>,
    /// When true, each pushed message is printed immediately (streamed) rather
    /// than collected and dumped later by `print_messages`.  Lets the user see
    /// load progress in real time instead of one burst after a silent transfer.
    pub live: bool,
}

/// Implementation of `MsgList`.
impl MsgList {
    /// Create new `MsgList`.
    pub const fn new() -> Self {
        Self {
            list: Vec::new(),
            live: false,
        }
    }

    /// Returns the number of messages of type `msg_type` in `MsgList`.
    pub fn number_by_type(&self, msg_type: MessageType) -> usize {
        self.list.iter().filter(|x| x.level == msg_type).count()
    }

    /// True if any error has been reported.
    pub fn has_errors(&self) -> bool {
        self.list.iter().any(|x| x.level == MessageType::Error)
    }

    /// Push message to `MsgList`.  In live mode the message is also printed
    /// immediately so the user sees progress in real time.
    pub fn push(&mut self, name: String, line_number: Option<u32>, file_name: Option<String>, msg_type: MessageType) {
        self.list.push(Message {
            text: name,
            line_number,
            file_name,
            level: msg_type,
            time: Some(Local::now().time()),
        });
        if self.live {
            if let Some(msg) = self.list.last() {
                /* Stream to stderr: it is unbuffered (no LineWriter), so each
                 * line shows immediately even with raw-mode / terminal quirks
                 * that left stdout lines stuck until the next forced flush. */
                eprintln!("{}", format_message(msg));
            }
        }
    }
}

/// Format one message as a coloured, timestamped line.
fn format_message(msg: &Message) -> String {
    let message_level: ColoredString = match msg.level {
        MessageType::Information => "I".to_owned().green(),
        MessageType::Warning => "W".to_owned().yellow(),
        MessageType::Error => "E".to_owned().red(),
    };
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
        format!("{} {} {} ", msg.time.unwrap_or_default().format("%H:%M:%S%.3f"), message_level, msg.text)
    }
}

/// Print out all messages.
///
/// Prints all the message in passed `MsgList` vector to terminal with coloured messages.
#[cfg(not(tarpaulin_include))] // Cannot test this function as it prints to terminal
pub fn print_messages(msg_list: &MsgList) {
    /* In live mode every message was already printed as it was pushed, so
     * printing them again here would duplicate the whole log. */
    if msg_list.live {
        return;
    }
    for msg in &msg_list.list {
        println!("{}", format_message(msg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Test that the message list is created correctly
    fn test_msg_list() {
        let mut msg_list = MsgList::new();
        msg_list.push("Test".to_owned(), None, Some("test".to_owned()), MessageType::Information);
        assert_eq!(msg_list.list.len(), 1);
        assert_eq!(msg_list.list[0].text, "Test");
        assert_eq!(msg_list.list[0].text, "Test");
        assert_eq!(msg_list.list[0].level, MessageType::Information);
        assert_eq!(msg_list.list[0].line_number, None);
    }

    #[test]
    // Test that the number of errors is correct
    fn test_number_errors() {
        let mut msg_list = MsgList::new();
        assert_eq!(msg_list.number_by_type(MessageType::Error), 0);
        msg_list.push("Test".to_owned(), None, None, MessageType::Information);
        msg_list.push("Test".to_owned(), None, None, MessageType::Warning);
        msg_list.push("Test".to_owned(), None, None, MessageType::Error);
        assert_eq!(msg_list.number_by_type(MessageType::Error), 1);
    }

    #[test]
    // Test that has_errors only counts errors
    fn test_has_errors() {
        let mut msg_list = MsgList::new();
        assert!(!msg_list.has_errors());
        msg_list.push("Test".to_owned(), None, None, MessageType::Information);
        msg_list.push("Test".to_owned(), None, None, MessageType::Warning);
        assert!(!msg_list.has_errors());
        msg_list.push("Test".to_owned(), None, None, MessageType::Error);
        assert!(msg_list.has_errors());
    }

    #[test]
    // Test number of warnings
    fn test_number_warnings() {
        let mut msg_list = MsgList::new();
        assert_eq!(msg_list.number_by_type(MessageType::Warning), 0);
        msg_list.push("Test".to_owned(), None, None, MessageType::Information);
        msg_list.push("Test".to_owned(), None, None, MessageType::Warning);
        msg_list.push("Test".to_owned(), None, None, MessageType::Error);
        assert_eq!(msg_list.number_by_type(MessageType::Warning), 1);
    }
}
