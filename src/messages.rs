

#[derive(Debug)]
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
