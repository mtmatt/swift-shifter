"""
自動評估指標模組。

- ChrF：字元 n-gram F 分數，快速、無需額外模型
- COMET：神經網路翻譯品質評估，與人工判斷相關性較高
  使用 Unbabel/wmt22-comet-da（需下載約 1.6GB 模型）
"""

from dataclasses import dataclass
from typing import Optional


@dataclass
class MetricScore:
    chrf: Optional[float] = None
    comet: Optional[float] = None
    comet_available: bool = False


def compute_chrf(hypothesis: str, reference: str) -> float:
    """計算單一句子的 ChrF 分數（0–100）"""
    try:
        from sacrebleu.metrics import CHRF
        metric = CHRF()
        result = metric.sentence_score(hypothesis, [reference])
        return round(result.score, 2)
    except ImportError:
        raise ImportError("請先安裝 sacrebleu：pip install sacrebleu")


def compute_chrf_corpus(hypotheses: list[str], references: list[str]) -> float:
    """計算整個測試集的 corpus-level ChrF"""
    try:
        from sacrebleu.metrics import CHRF
        metric = CHRF()
        result = metric.corpus_score(hypotheses, [references])
        return round(result.score, 2)
    except ImportError:
        raise ImportError("請先安裝 sacrebleu：pip install sacrebleu")


def load_comet_model(model_name: str = "Unbabel/wmt22-comet-da"):
    """載入 COMET 模型（第一次執行會下載）"""
    try:
        from comet import download_model, load_from_checkpoint
        path = download_model(model_name)
        return load_from_checkpoint(path)
    except ImportError:
        raise ImportError("請先安裝 comet-score：pip install unbabel-comet")


def compute_comet(
    sources: list[str],
    hypotheses: list[str],
    references: list[str],
    comet_model=None,
    batch_size: int = 8,
) -> list[float]:
    """
    計算 COMET 分數，回傳每個樣本的分數列表。
    comet_model 可傳入已載入的模型以避免重複載入。
    """
    if comet_model is None:
        comet_model = load_comet_model()

    data = [
        {"src": s, "mt": h, "ref": r}
        for s, h, r in zip(sources, hypotheses, references)
    ]
    results = comet_model.predict(data, batch_size=batch_size, gpus=1)
    return [round(float(s), 4) for s in results.scores]


def score_sample(
    source: str,
    hypothesis: str,
    reference: Optional[str],
    comet_model=None,
) -> MetricScore:
    """對單一樣本計算所有可用指標"""
    if not reference or not hypothesis:
        return MetricScore()

    chrf = compute_chrf(hypothesis, reference)

    comet_score = None
    comet_available = False
    if comet_model is not None:
        try:
            scores = compute_comet([source], [hypothesis], [reference], comet_model)
            comet_score = scores[0]
            comet_available = True
        except Exception:
            pass

    return MetricScore(chrf=chrf, comet=comet_score, comet_available=comet_available)


def score_sample_multi_ref(
    source: str,
    hypothesis: str,
    references: dict[str, str],
    comet_model=None,
) -> dict[str, MetricScore]:
    """
    對多個參考譯文分別計算指標。
    回傳 dict，key 為參考譯文來源名稱（如 "claude"、"gemini"）。
    """
    if not hypothesis or not references:
        return {}

    return {
        ref_name: score_sample(source, hypothesis, ref_text, comet_model)
        for ref_name, ref_text in references.items()
        if ref_text
    }
