# translation_eval

小模型翻譯品質評估框架，支援 llama.cpp（GGUF）和 transformers（HF）雙後端，
以及多參考譯文評分（例如同時用 Claude 和 Gemini 的翻譯作為參考）。

## 安裝

```bash
pip install sacrebleu unbabel-comet datasets transformers accelerate

# 如果要跑 GGUF 模型
pip install llama-cpp-python
```

## 使用方式

### 1. 準備參考譯文

在 `testsets/custom_template.json` 的 `references` 欄位填入各模型的翻譯：

```json
{
  "id": "biz_001",
  "source_lang": "Chinese",
  "target_lang": "English",
  "source_text": "本合約自雙方簽署之日起生效...",
  "reference": null,
  "references": {
    "claude": "This Agreement shall come into effect...",
    "gemini": "This Agreement shall take effect..."
  },
  "domain": "business",
  "notes": "注意術語"
}
```

`references` 留空（`{}`）的樣本不會計算自動指標，但仍會翻譯並輸出供人工審閱。

### 2. 編輯設定檔

複製 `config_example.json`，填入你的模型路徑：

```json
{
  "models": [
    {"model_id": "/path/to/model.gguf", "n_gpu_layers": -1},
    {"model_id": "Qwen/Qwen2.5-7B-Instruct"}
  ],
  "flores200": {
    "source_lang": "zho_Hant",
    "target_lang": "eng_Latn",
    "max_samples": 50
  },
  "custom_samples": "testsets/custom_template.json",
  "use_comet": true
}
```

### 3. 執行評估

```bash
python evaluate.py --config config_example.json --output results.json
```

### 4. 結果格式

輸出的 `results.json` 中，每個樣本包含：

```json
{
  "sample_id": "biz_001",
  "translation": "...",
  "metrics_per_ref": {
    "claude": {"chrf": 72.3, "comet": 0.8812},
    "gemini": {"chrf": 68.1, "comet": 0.8654}
  },
  "human_review": {
    "accuracy": null,
    "fluency": null,
    "terminology": null,
    "comments": ""
  }
}
```

摘要統計（`summary.scores_per_ref`）也會對每個參考來源各自列出平均分：

```json
"scores_per_ref": {
  "claude":  {"avg_chrf": 71.2, "avg_comet": 0.8790, "n": 8},
  "gemini":  {"avg_chrf": 67.4, "avg_comet": 0.8621, "n": 8}
}
```

### 5. 分數解讀

**ChrF（0–100）**：字元 n-gram 重疊，模型間相對比較用，差距 2–3 分以上才有意義。

**COMET（0.7–0.9）**：神經網路評估，與人工判斷相關性較高。參考範圍：
- 0.85 以上：接近人工翻譯水準
- 0.80–0.85：商業使用邊界
- 0.75 以下：需要大量後編輯

兩個參考的分數若同向（對 claude 高、對 gemini 也高），品質判斷比較可靠；若只對其中一個高，可能是用詞風格差異而非品質問題，這種樣本優先人工審閱。

### 6. 人工審閱

在輸出的 JSON 中填入 `human_review` 欄位（各 1–5 分）：

```json
"human_review": {
  "accuracy": 4,
  "fluency": 3,
  "terminology": 5,
  "comments": "「material breach」譯為「重大違約」正確，但語序略顯生硬"
}
```

## FLORES-200 語言代碼

| 語言     | 代碼       |
|----------|------------|
| 中文繁體 | zho_Hant   |
| 中文簡體 | zho_Hans   |
| 英文     | eng_Latn   |
| 日文     | jpn_Jpan   |
| 德文     | deu_Latn   |
| 法文     | fra_Latn   |
| 韓文     | kor_Hang   |

完整代碼列表：https://huggingface.co/datasets/facebook/flores

## 檔案結構

```
translation_eval/
├── evaluate.py              # 主評估流程
├── testset.py               # 測試集管理
├── metrics.py               # ChrF / COMET 計算
├── config_example.json      # 設定檔範例
├── backends/
│   ├── __init__.py          # auto_backend 自動選擇後端
│   ├── base.py              # 後端基礎類別
│   ├── llamacpp_backend.py  # llama.cpp（GGUF）後端
│   └── transformers_backend.py  # HuggingFace 後端
└── testsets/
    └── custom_template.json # 自訂樣本範本（含多參考欄位）
```

