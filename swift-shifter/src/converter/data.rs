use std::path::{Path, PathBuf};

fn output_path(input: &str, ext: &str, output_dir: Option<&str>) -> Result<PathBuf, String> {
    let p = Path::new(input);
    let stem = p.file_stem().unwrap_or_default();
    let dir = match output_dir {
        Some(d) => {
            let dir = PathBuf::from(d);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create output directory: {e}"))?;
            dir
        }
        None => p.parent().unwrap_or(Path::new(".")).to_path_buf(),
    };
    Ok(dir.join(format!("{}.{}", stem.to_string_lossy(), ext)))
}

pub fn convert_data(path: &str, target_format: &str, output_dir: Option<&str>) -> Result<String, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {e}"))?;
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Parse input into a serde_json::Value as the intermediate representation
    let value: serde_json::Value = match ext.as_str() {
        "json" => serde_json::from_str(&content).map_err(|e| format!("Invalid JSON: {e}"))?,
        "yaml" | "yml" => {
            serde_yaml::from_str(&content).map_err(|e| format!("Invalid YAML: {e}"))?
        }
        "toml" => {
            let toml_val: toml::Value =
                toml::from_str(&content).map_err(|e| format!("Invalid TOML: {e}"))?;
            // Convert toml::Value to serde_json::Value
            serde_json::to_value(toml_val).map_err(|e| format!("TOML→JSON conversion: {e}"))?
        }
        "csv" => {
            let mut rdr = csv::Reader::from_reader(content.as_bytes());
            let headers = rdr
                .headers()
                .map_err(|e| format!("CSV headers error: {e}"))?
                .clone();
            let mut rows = Vec::new();
            for result in rdr.records() {
                let record = result.map_err(|e| format!("CSV record error: {e}"))?;
                let obj: serde_json::Map<String, serde_json::Value> = headers
                    .iter()
                    .zip(record.iter())
                    .map(|(h, v)| (h.to_string(), serde_json::Value::String(v.to_string())))
                    .collect();
                rows.push(serde_json::Value::Object(obj));
            }
            serde_json::Value::Array(rows)
        }
        other => return Err(format!("Unsupported data format: .{other}")),
    };

    // Serialize to target format
    let out = output_path(path, target_format, output_dir)?;
    let output_content = match target_format {
        "json" => serde_json::to_string_pretty(&value)
            .map_err(|e| format!("JSON serialization error: {e}"))?,
        "yaml" => {
            serde_yaml::to_string(&value).map_err(|e| format!("YAML serialization error: {e}"))?
        }
        "toml" => {
            let toml_val: toml::Value = serde_json::from_value(value)
                .map_err(|e| format!("JSON→TOML conversion: {e}"))?;
            toml::to_string_pretty(&toml_val)
                .map_err(|e| format!("TOML serialization error: {e}"))?
        }
        "csv" => {
            // value should be an array of objects
            let rows = match &value {
                serde_json::Value::Array(arr) => arr,
                _ => return Err("CSV output requires an array of objects".to_string()),
            };
            if rows.is_empty() {
                String::new()
            } else {
                let headers: Vec<String> = match &rows[0] {
                    serde_json::Value::Object(map) => map.keys().cloned().collect(),
                    _ => return Err("CSV output requires an array of objects".to_string()),
                };
                let mut wtr = csv::Writer::from_writer(Vec::new());
                wtr.write_record(&headers)
                    .map_err(|e| format!("CSV write error: {e}"))?;
                for row in rows {
                    if let serde_json::Value::Object(map) = row {
                        let record: Vec<String> = headers
                            .iter()
                            .map(|h| {
                                map.get(h)
                                    .map(|v| match v {
                                        serde_json::Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default()
                            })
                            .collect();
                        wtr.write_record(&record)
                            .map_err(|e| format!("CSV write error: {e}"))?;
                    }
                }
                String::from_utf8(wtr.into_inner().map_err(|e| format!("CSV flush error: {e}"))?)
                    .map_err(|e| format!("CSV encoding error: {e}"))?
            }
        }
        other => return Err(format!("Unsupported target format: {other}")),
    };

    std::fs::write(&out, output_content)
        .map_err(|e| format!("Failed to write output file: {e}"))?;
    Ok(out.to_string_lossy().to_string())
}
