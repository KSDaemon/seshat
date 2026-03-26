// User model
pub struct User {
    pub name: String,
    pub email: String,
}

impl User {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            email: String::new(),
        }
    }
}
