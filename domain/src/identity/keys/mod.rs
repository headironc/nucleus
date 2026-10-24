//! Identity key sets, each built from one use derived from the application root.

pub mod avatar;
pub mod challenge;
pub mod credential;
pub mod encryption;
pub mod message;
pub mod mobile;
pub mod number;
pub mod request;

pub struct Keys {
    pub challenge: challenge::Keys,
    pub mobile: mobile::Keys,
    pub encryption: encryption::Keys,
    pub message: message::Keys,
    pub input: request::Keys,
    pub credential: credential::Keys,
    pub number: number::Keys,
    pub avatar: avatar::Keys,
}
