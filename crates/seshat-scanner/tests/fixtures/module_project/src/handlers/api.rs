// API handler — imports from models and utils
use crate::models::User;
use crate::utils::format_response;

pub fn handle_request(user: &User) -> String {
    format_response(&user.name)
}
