#[derive(serde::Deserialize, Clone)]
pub struct LlmCfg {
    pub enabled: bool,
    pub model: String,
    pub url: String,
}

#[derive(serde::Serialize, Clone)]
pub struct ProgressPayload {
    pub path: String,
    pub percent: f32,
}
