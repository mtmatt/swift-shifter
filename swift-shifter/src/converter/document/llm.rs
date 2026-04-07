use tauri::Emitter;
use crate::converter::document::{OLLAMA_CLIENT};
use crate::converter::document::binaries::{find_any_binary, run_silent};

#[cfg(target_os = "macos")]
use crate::converter::document::binaries::find_brew_binary;

/// Returns true if the Ollama server is reachable at the given base URL.
pub async fn ollama_reachable(base_url: &str) -> bool {
    let client = OLLAMA_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("reqwest client with timeout should always build")
    });
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    client.get(url).send().await.map(|r| r.status().is_success()).unwrap_or(false)
}

/// Lists all models currently pulled on the local Ollama server.
pub async fn ollama_list_models(base_url: &str) -> Vec<String> {
    let client = OLLAMA_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("reqwest client with timeout should always build")
    });
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let Ok(resp) = client.get(url).send().await else { return vec![] };
    let Ok(json) = resp.json::<serde_json::Value>().await else { return vec![] };
    
    json["models"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Instruct a local LLM to fix indentation, rejoin hyphenated line-breaks,
/// and repair math notation in the generated Markdown.
pub async fn llm_postprocess_markdown(
    app: &tauri::AppHandle,
    markdown: String,
    input_path: &str,
    base_url: &str,
    model: &str,
) -> String {
    if markdown.chars().count() > 12_000 {
        return markdown;
    }

    let client = OLLAMA_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("reqwest client with timeout should always build")
    });

    let prompt = build_llm_prompt(&markdown);
    let url = format!("{}/api/generate", base_url.trim_end_matches('/'));

    let payload = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "stream": true,
    });

    let resp = match client.post(url).json(&payload).send().await {
        Ok(r) => r,
        Err(_) => return markdown,
    };

    if !resp.status().is_success() {
        return markdown;
    }

    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();

    while let Some(item) = stream.next().await {
        let Ok(chunk) = item else { break };
        for line in String::from_utf8_lossy(&chunk).lines() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(token) = json["response"].as_str() {
                    full_response.push_str(token);
                    app.emit("llm:progress", serde_json::json!({
                        "path": input_path,
                        "token": token,
                    })).ok();
                }
                if json["done"].as_bool() == Some(true) {
                    break;
                }
            }
        }
    }

    if full_response.trim().is_empty() {
        return markdown;
    }

    full_response
        .trim()
        .trim_start_matches("```markdown")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string()
}

pub fn build_llm_prompt(markdown: &str) -> String {
    format!(
"Fix the following Markdown document. 

Instructions:
1. Output ONLY the corrected Markdown. No commentary, no preamble, no 'Here is the fix'.
2. Preserve all content exactly. Do not summarize, reorder, or remove any text.
3. Fix code block indentation. If a code block is flat, indent nested blocks (e.g. inside functions/classes) with 4 spaces per level.
4. Rejoin hyphenated line-break words (e.g. 'auto-matic' at the end of a line should become 'automatic').
5. Ensure LaTeX math is correctly wrapped in $...$ for inline or $$...$$ for blocks.

Document:
```markdown
{}
```", markdown)
}

pub async fn install_ollama_and_model(
    app: &tauri::AppHandle,
    base_url: &str,
    model: &str,
) -> Result<Option<tokio::process::Child>, String> {
    let ollama_bin = find_any_binary(&["ollama", "ollama.exe"]);

    if ollama_bin.is_none() {
        app.emit("ollama:step", "Installing Ollama…").ok();
        #[cfg(target_os = "macos")]
        {
            if let Some(brew) = find_brew_binary() {
                run_silent(&brew, &["install", "ollama"]).await.ok();
                run_silent(&brew, &["services", "start", "ollama"]).await.ok();
            }
        }
        #[cfg(target_os = "windows")]
        {
            if which::which("winget").is_ok() {
                run_silent(
                    &std::path::PathBuf::from("winget"),
                    &["install", "--id", "Ollama.Ollama", "-e", "--silent"],
                )
                .await
                .ok();
            }
        }
        #[cfg(target_os = "linux")]
        {
            // Automated install via curl|sh is a security risk and violates project policy.
            // Instruct the user to install Ollama manually.
            app.emit("ollama:step",
                "Ollama not found. On Linux, install manually: https://ollama.com/download/linux"
            ).ok();
        }
    }

    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    let mut reachable = ollama_reachable(base_url).await;
    let mut child_handle = None;
    if !reachable {
        app.emit("ollama:step", "Starting Ollama server…").ok();
        if let Some(bin) = find_any_binary(&["ollama", "ollama.exe"]) {
            let mut cmd = tokio::process::Command::new(&bin);
            cmd.arg("serve");
            // Prevent child from inheriting stdout/stderr which could keep app alive or spam logs
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            match cmd.spawn() {
                Ok(child) => {
                    child_handle = Some(child);
                    for _ in 0..15 {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        if ollama_reachable(base_url).await {
                            reachable = true;
                            break;
                        }
                    }
                }
                Err(e) => return Err(format!("Failed to start Ollama: {}", e)),
            }
        }
    }

    if !reachable {
        return Err("Ollama server is not reachable and could not be started automatically.".to_string());
    }

    app.emit("ollama:step", format!("Pulling model {}…", model)).ok();

    let client = OLLAMA_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("reqwest client with timeout should always build")
    });

    let url = format!("{}/api/pull", base_url.trim_end_matches('/'));
    let payload = serde_json::json!({
        "model": model,
        "stream": true,
    });

    let resp = client.post(&url).json(&payload).send().await
        .map_err(|e| format!("Failed to connect to Ollama: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Ollama API returned {}", resp.status()));
    }

    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();

    while let Some(item) = stream.next().await {
        let Ok(chunk) = item else { break };
        for line in String::from_utf8_lossy(&chunk).lines() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                if let (Some(completed), Some(total)) = (json["completed"].as_f64(), json["total"].as_f64()) {
                    if total > 0.0 {
                        let pct = (completed / total * 100.0) as f32;
                        app.emit("ollama:progress", pct).ok();
                    }
                }
            }
        }
    }

    app.emit("ollama:step", "Done!").ok();
    app.emit("ollama:progress", 100.0).ok();

    Ok(child_handle)
}
