// Web handler — imports from models
use crate::models::User;

pub fn render_page(user: &User) -> String {
    format!("<h1>{}</h1>", user.name)
}
