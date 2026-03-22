"""
測試集管理模組。

提供兩種測試集來源：
1. FLORES-200（公開基準，有標準參考譯文）
2. 自訂領域樣本（商業/技術文件，無標準答案，供人工審閱）
"""

from dataclasses import dataclass, field
from typing import Optional
import json
from pathlib import Path


@dataclass
class TestSample:
    id: str
    source_lang: str
    target_lang: str
    source_text: str
    reference: Optional[str] = None          # 單一參考譯文（向下相容）
    references: dict[str, str] = field(default_factory=dict)  # 多個參考譯文，key 為來源名稱
    domain: str = "general"                  # general / business / technical
    notes: str = ""                          # 給人工審閱的備註

    def all_references(self) -> dict[str, str]:
        """回傳所有參考譯文，合併 reference 和 references"""
        result = dict(self.references)
        if self.reference and "default" not in result:
            result["default"] = self.reference
        return result


def load_flores200(
    source_lang: str,
    target_lang: str,
    split: str = "devtest",
    max_samples: int = 50,
) -> list[TestSample]:
    """
    從 HuggingFace datasets 載入 FLORES-200。
    語言代碼使用 FLORES 格式，例如：
        中文繁體 = zho_Hant
        中文簡體 = zho_Hans
        英文     = eng_Latn
    """
    try:
        from datasets import load_dataset
    except ImportError:
        raise ImportError("請先安裝 datasets：pip install datasets")

    src_ds = load_dataset("facebook/flores", source_lang, split=split, trust_remote_code=True)
    tgt_ds = load_dataset("facebook/flores", target_lang, split=split, trust_remote_code=True)

    samples = []
    for i, (src_row, tgt_row) in enumerate(zip(src_ds, tgt_ds)):
        if i >= max_samples:
            break
        samples.append(TestSample(
            id=f"flores_{i:04d}",
            source_lang=source_lang,
            target_lang=target_lang,
            source_text=src_row["sentence"],
            reference=tgt_row["sentence"],
            domain="general",
        ))
    return samples


def load_custom_samples(path: str) -> list[TestSample]:
    """
    從 JSON 檔案載入自訂測試樣本。
    格式範例請見 testsets/custom_template.json。
    """
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    samples = []
    for item in data:
        samples.append(TestSample(
            id=item["id"],
            source_lang=item["source_lang"],
            target_lang=item["target_lang"],
            source_text=item["source_text"],
            reference=item.get("reference"),
            references=item.get("references", {}),
            domain=item.get("domain", "general"),
            notes=item.get("notes", ""),
        ))
    return samples


def save_samples(samples: list[TestSample], path: str) -> None:
    data = [
        {
            "id": s.id,
            "source_lang": s.source_lang,
            "target_lang": s.target_lang,
            "source_text": s.source_text,
            "reference": s.reference,
            "references": s.references,
            "domain": s.domain,
            "notes": s.notes,
        }
        for s in samples
    ]
    Path(path).write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"已儲存 {len(samples)} 筆樣本到 {path}")
