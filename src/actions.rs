use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Action {
    /// Clerk-assigned opaque id. Empty fails closed on `Store::apply_action`.
    pub id: String,
    pub kind: String,
    pub key: String,
    pub props: HashMap<String, String>,
}
