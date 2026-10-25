//! Diagnostics: the errors, warnings and notes reported to the user.
//!
//! Problems in the user's source are collected in a [`Diagnostics`] so one run
//! can report all of them; a command fails on [`Diagnostics::has_errors`].  A
//! streaming list ([`Diagnostics::streaming`]) also prints each message as it
//! is reported, so the user sees progress in real time.

use chrono::{Local, NaiveTime};
use colored::{ColoredString, Colorize as _};
use std::fmt;
use std::time::Instant;

#[derive(Debug)]
/// One reported message.
pub(crate) struct Message {
    /// File name of file causing message if exists.
    file_name: Option<String>,
    /// Message type.
    level: MessageType,
    /// Line number in file causing message if exists.
    line_number: Option<u32>,
    /// Text of message.
    text: String,
    /// Time of message.
    time: NaiveTime,
}

impl Message {
    /// The text of the message, without its location.
    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// How serious the message is.
    #[cfg(test)]
    pub(crate) const fn level(&self) -> MessageType {
        self.level
    }

    /// The line the message is about, if any.
    #[cfg(test)]
    pub(crate) const fn line_number(&self) -> Option<u32> {
        self.line_number
    }

    /// The file the message is about, if any.
    #[cfg(test)]
    pub(crate) fn file_name(&self) -> Option<&str> {
        self.file_name.as_deref()
    }
}

/// A coloured, timestamped line: the time, the level letter, the location (if
/// any) and the text, with a trailing space.
impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level: ColoredString = match self.level {
            MessageType::Information => "I".green(),
            MessageType::Warning => "W".yellow(),
            MessageType::Error => "E".red(),
        };
        write!(f, "{} {level} ", self.time.format("%H:%M:%S%.3f"))?;
        match (self.line_number, &self.file_name) {
            (Some(line), Some(file)) => write!(f, "Line {line} in file {file}. ")?,
            (Some(line), None) => write!(f, "Line {line}. ")?,
            (None, Some(file)) => write!(f, "In file {file}. ")?,
            (None, None) => {}
        }
        write!(f, "{} ", self.text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Enum for message type.
pub(crate) enum MessageType {
    /// Error message.
    Error,
    /// Information message.
    Information,
    /// Warning message.
    Warning,
}

#[derive(Debug)]
/// The messages reported so far.
pub(crate) struct Diagnostics {
    /// The messages, in the order they were reported.
    list: Vec<Message>,
    /// When true, each pushed message is printed immediately (streamed) rather
    /// than collected and dumped later by `print_messages`.  Lets the user see
    /// load progress in real time instead of one burst after a silent transfer.
    live: bool,
    /// When the run started, for the summary's elapsed time.
    started: Instant,
    /// The summary has been printed (it is printed at most once).
    summary_printed: bool,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl Diagnostics {
    /// An empty list that collects its messages for [`print_messages`].
    pub(crate) fn new() -> Self {
        Self {
            list: Vec::new(),
            live: false,
            started: Instant::now(),
            summary_printed: false,
        }
    }

    /// An empty list that prints each message (on stderr) as it is reported.
    pub(crate) fn streaming() -> Self {
        Self { live: true, ..Self::new() }
    }

    /// The messages reported so far, oldest first.
    #[cfg(test)]
    pub(crate) fn messages(&self) -> &[Message] {
        &self.list
    }

    /// The number of messages of `level`.
    pub(crate) fn count(&self, level: MessageType) -> usize {
        self.list.iter().filter(|x| x.level == level).count()
    }

    /// True if any error has been reported.
    pub(crate) fn has_errors(&self) -> bool {
        self.list.iter().any(|x| x.level == MessageType::Error)
    }

    /// Report an error that is not about a source line.
    pub(crate) fn error(&mut self, text: impl Into<String>) {
        self.push(text.into(), None, None, MessageType::Error);
    }

    /// Report a warning that is not about a source line.
    pub(crate) fn warning(&mut self, text: impl Into<String>) {
        self.push(text.into(), None, None, MessageType::Warning);
    }

    /// Report a note (progress, or what was done).
    pub(crate) fn info(&mut self, text: impl Into<String>) {
        self.push(text.into(), None, None, MessageType::Information);
    }

    /// Report an error about line `line_number` of `file_name`.
    pub(crate) fn error_at(&mut self, file_name: impl Into<String>, line_number: u32, text: impl Into<String>) {
        self.push(text.into(), Some(line_number), Some(file_name.into()), MessageType::Error);
    }

    /// Report a warning about line `line_number` of `file_name`.
    pub(crate) fn warning_at(&mut self, file_name: impl Into<String>, line_number: u32, text: impl Into<String>) {
        self.push(text.into(), Some(line_number), Some(file_name.into()), MessageType::Warning);
    }

    /// Push message to `Diagnostics`.  In live mode the message is also printed
    /// immediately so the user sees progress in real time.
    pub(crate) fn push(&mut self, text: String, line_number: Option<u32>, file_name: Option<String>, level: MessageType) {
        let msg = Message {
            text,
            line_number,
            file_name,
            level,
            time: Local::now().time(),
        };
        if self.live {
            /* Stream to stderr: it is unbuffered (no LineWriter), so each
             * line shows immediately even with raw-mode / terminal quirks
             * that left stdout lines stuck until the next forced flush. */
            eprintln!("{msg}");
        }
        self.list.push(msg);
    }

    /// Print the run summary: the error and warning counts and the time since
    /// the list was made (after the collected messages, for a list that is not
    /// streaming).
    ///
    /// Only the first call prints.  `run` prints the summary when the command
    /// is done; a command that then hands the terminal to something that runs
    /// until the user stops it (the serial monitor, a debugger session) or
    /// prints a report of its own prints it before that.
    #[cfg(not(tarpaulin_include))] // Cannot test printing in tarpaulin
    pub(crate) fn print_summary(&mut self) {
        if std::mem::replace(&mut self.summary_printed, true) {
            return;
        }
        print_messages(self);
        eprintln!("{}", self.summary());
    }

    /// The summary line [`print_summary`](Self::print_summary) prints.
    fn summary(&self) -> String {
        let errors = self.count(MessageType::Error);
        let warnings = self.count(MessageType::Warning);
        format!(
            "Completed with {errors} error{} and {warnings} warning{} in {:.3} seconds",
            if errors == 1 { "" } else { "s" },
            if warnings == 1 { "" } else { "s" },
            self.started.elapsed().as_secs_f64(),
        )
    }
}

/// Print out all messages.
///
/// Prints all the message in passed `Diagnostics` vector to terminal with coloured messages.
#[cfg(not(tarpaulin_include))] // Cannot test this function as it prints to terminal
pub(crate) fn print_messages(msg_list: &Diagnostics) {
    /* In live mode every message was already printed as it was pushed, so
     * printing them again here would duplicate the whole log. */
    if msg_list.live {
        return;
    }
    for msg in &msg_list.list {
        println!("{msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Test that the message list is created correctly
    fn test_msg_list() {
        let mut msg_list = Diagnostics::new();
        msg_list.push("Test".to_owned(), None, Some("test".to_owned()), MessageType::Information);
        assert_eq!(msg_list.messages().len(), 1);
        assert_eq!(msg_list.messages()[0].text(), "Test");
        assert_eq!(msg_list.messages()[0].level(), MessageType::Information);
        assert_eq!(msg_list.messages()[0].line_number(), None);
        assert_eq!(msg_list.messages()[0].file_name(), Some("test"));
    }

    #[test]
    fn helpers_set_the_level_and_location() {
        let mut msg_list = Diagnostics::new();
        msg_list.info("note");
        msg_list.warning("careful");
        msg_list.error("broken");
        msg_list.warning_at("a.kla", 3, "odd");
        msg_list.error_at("b.kla", 7, "bad");
        let got: Vec<_> = msg_list
            .messages()
            .iter()
            .map(|m| (m.level(), m.file_name(), m.line_number(), m.text()))
            .collect();
        assert_eq!(
            got,
            [
                (MessageType::Information, None, None, "note"),
                (MessageType::Warning, None, None, "careful"),
                (MessageType::Error, None, None, "broken"),
                (MessageType::Warning, Some("a.kla"), Some(3), "odd"),
                (MessageType::Error, Some("b.kla"), Some(7), "bad"),
            ]
        );
        assert_eq!(msg_list.count(MessageType::Warning), 2);
        assert_eq!(msg_list.count(MessageType::Error), 2);
        assert_eq!(msg_list.count(MessageType::Information), 1);
    }

    #[test]
    fn summary_counts_errors_and_warnings() {
        let mut msg_list = Diagnostics::new();
        let summary = msg_list.summary();
        assert!(summary.starts_with("Completed with 0 errors and 0 warnings in "), "{summary}");
        assert!(summary.ends_with(" seconds"), "{summary}");
        msg_list.error("e");
        msg_list.warning("w");
        assert!(msg_list.summary().starts_with("Completed with 1 error and 1 warning in "));
        msg_list.error("e");
        msg_list.warning("w");
        msg_list.info("i");
        assert!(msg_list.summary().starts_with("Completed with 2 errors and 2 warnings in "));
    }

    /// A message at 12:34:56.789 with the given location.
    fn message(level: MessageType, line_number: Option<u32>, file_name: Option<&str>) -> Message {
        Message {
            file_name: file_name.map(str::to_owned),
            level,
            line_number,
            text: "Text".to_owned(),
            time: NaiveTime::from_hms_milli_opt(12, 34, 56, 789).unwrap(),
        }
    }

    #[test]
    fn display_shows_time_level_location_and_text() {
        colored::control::set_override(false);
        assert_eq!(
            message(MessageType::Error, Some(4), Some("a.kla")).to_string(),
            "12:34:56.789 E Line 4 in file a.kla. Text "
        );
        assert_eq!(message(MessageType::Warning, Some(4), None).to_string(), "12:34:56.789 W Line 4. Text ");
        assert_eq!(
            message(MessageType::Information, None, Some("a.kla")).to_string(),
            "12:34:56.789 I In file a.kla. Text "
        );
        assert_eq!(message(MessageType::Information, None, None).to_string(), "12:34:56.789 I Text ");
    }

    #[test]
    // Test that the number of errors is correct
    fn test_number_errors() {
        let mut msg_list = Diagnostics::new();
        assert_eq!(msg_list.count(MessageType::Error), 0);
        msg_list.info("Test");
        msg_list.warning("Test");
        msg_list.error("Test");
        assert_eq!(msg_list.count(MessageType::Error), 1);
    }

    #[test]
    // Test that has_errors only counts errors
    fn test_has_errors() {
        let mut msg_list = Diagnostics::new();
        assert!(!msg_list.has_errors());
        msg_list.info("Test");
        msg_list.warning("Test");
        assert!(!msg_list.has_errors());
        msg_list.error("Test");
        assert!(msg_list.has_errors());
    }

    #[test]
    // Test number of warnings
    fn test_number_warnings() {
        let mut msg_list = Diagnostics::new();
        assert_eq!(msg_list.count(MessageType::Warning), 0);
        msg_list.info("Test");
        msg_list.warning("Test");
        msg_list.error("Test");
        assert_eq!(msg_list.count(MessageType::Warning), 1);
    }
}
