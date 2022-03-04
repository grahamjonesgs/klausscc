#[derive(Debug)]
#[derive(PartialEq)]
pub enum MessageType {
    Error,
    Warning,
    Info
}

#[derive(Debug)]
pub struct Message {
    pub name: String,
    pub number: u32,
    pub level: MessageType,
}

pub fn add_message(name: &str, msg_type: MessageType, number: u32, msgs: &mut Vec<Message>) {
    let new_message = Message {
        name: String::from(name),
        number: number,
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
