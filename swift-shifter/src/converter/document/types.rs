#[derive(serde::Serialize, Clone)]
pub struct ProgressPayload {
    pub path: String,
    pub percent: f32,
}
