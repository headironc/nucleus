//! Validated C1 JSON values and their binding to the request context.

use std::net::IpAddr;

use domain::identity::{authentication, challenge::Token, mobile::Mobile, operation};
use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Input {
    #[schema(value_type = String, pattern = "^(?:13[0-9]|14[01456879]|15[0-35-9]|16[2567]|17[0-8]|18[0-9]|19[0-35-9])[0-9]{8}$")]
    mobile: Mobile,
    #[schema(value_type = String, min_length = 1, max_length = 2048)]
    token: Token,
}

impl Input {
    pub(super) fn into_command(
        self,
        operation_id: operation::Id,
        source: IpAddr,
    ) -> authentication::Input {
        authentication::Input::new(operation_id, self.mobile, self.token, source)
    }
}
