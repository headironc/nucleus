//! Closed, server-selected authentication sources; transport follows the source.

use crate::identity::{mini, website};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    WebsiteMessage(website::AppId),
    MiniMobile(mini::AppId),
}

impl Origin {
    pub fn method(&self) -> &'static str {
        match self {
            Self::WebsiteMessage(_) => "message",
            Self::MiniMobile(_) => "mobile",
        }
    }

    pub fn transport(&self) -> &'static str {
        match self {
            Self::WebsiteMessage(_) => "cookie",
            Self::MiniMobile(_) => "bearer",
        }
    }

    pub fn app_id(&self) -> &str {
        match self {
            Self::WebsiteMessage(id) => id.as_ref(),
            Self::MiniMobile(id) => id.as_ref(),
        }
    }

    pub(crate) fn restore(method: &str, app_id: &str) -> Result<Self, website::Error> {
        match method {
            "message" => Ok(Self::WebsiteMessage(app_id.parse()?)),
            "mobile" => Ok(Self::MiniMobile(app_id.parse()?)),
            _ => Err(website::Error),
        }
    }
}
