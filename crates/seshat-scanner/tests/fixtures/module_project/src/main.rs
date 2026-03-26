// Entry point — imports from handlers and models
use crate::handlers::api;
use crate::models::User;

fn main() {
    let user = User::new("alice");
    api::handle_request(&user);
}
