"""
主評估流程

使用方式：
    python evaluate.py --config config_example.json

或直接在程式中呼叫 run_evaluation()。
"""

import argparse
import json
import time
from dataclasses import asdict
from datetime import datetime
from pathlib import Path
from typing import Optional

from backends import auto_backend
from metrics import MetricScore, compute_chrf_corpus, load_comet_model, score_sample, score_sample_multi_ref
from testset import TestSample, load_custom_samples, load_flores200


def run_evaluation(
    models: list[dict],
    samples: list[TestSample],
    output_path: str = "results.json",
    use_comet: bool = True,
    comet_batch_size: int = 8,
) -> dict:
    """
    對所有模型跑完整評估，輸出結果 JSON。

    models 格式：
        [
            {"model_id": "/models/gemma-3n-e4b.gguf"},
            {"model_id": "Qwen/Qwen2.5-7B-Instruct", "dtype": "bfloat16"},
        ]
    """
    comet_model = None
    if use_comet:
        print("載入 COMET 模型...")
        try:
            comet_model = load_comet_model()
            print("COMET 模型載入完成")
        except Exception as e:
            print(f"警告：COMET 載入失敗（{e}），將只計算 ChrF")

    run_meta = {
        "timestamp": datetime.now().isoformat(),
        "total_samples": len(samples),
        "models_evaluated": len(models),
    }

    all_results = []

    for model_cfg in models:
        model_id = model_cfg["model_id"]
        backend_kwargs = {k: v for k, v in model_cfg.items() if k != "model_id"}

        print(f"\n{'='*60}")
        print(f"評估模型：{model_id}")
        print(f"{'='*60}")

        model_results = {
            "model_id": model_id,
            "backend_kwargs": backend_kwargs,
            "translations": [],
            "summary": {},
        }

        try:
            backend = auto_backend(model_id, **backend_kwargs)
            backend.load()
            print(f"後端：{backend.__class__.__name__}")

            t_model_start = time.perf_counter()

            for i, sample in enumerate(samples):
                print(f"  [{i+1}/{len(samples)}] {sample.id} ({sample.domain})", end="", flush=True)

                result = backend.translate(
                    sample.source_text,
                    source_lang=sample.source_lang,
                    target_lang=sample.target_lang,
                )

                all_refs = sample.all_references()

                if all_refs:
                    multi_scores = score_sample_multi_ref(
                        source=sample.source_text,
                        hypothesis=result.translation,
                        references=all_refs,
                        comet_model=comet_model,
                    )
                else:
                    multi_scores = {}

                # 為了向下相容，也保留單一 metrics 欄位（取第一個參考）
                first_score = next(iter(multi_scores.values()), MetricScore())

                entry = {
                    "sample_id": sample.id,
                    "domain": sample.domain,
                    "source_lang": sample.source_lang,
                    "target_lang": sample.target_lang,
                    "source_text": sample.source_text,
                    "references": all_refs,
                    "notes": sample.notes,
                    "translation": result.translation,
                    "elapsed_sec": result.elapsed_sec,
                    "tokens_per_sec": result.tokens_per_sec,
                    "error": result.error,
                    # 每個參考譯文的分數
                    "metrics_per_ref": {
                        ref_name: {"chrf": s.chrf, "comet": s.comet}
                        for ref_name, s in multi_scores.items()
                    },
                    # 向下相容的單一指標欄位
                    "metrics": {
                        "chrf": first_score.chrf,
                        "comet": first_score.comet,
                    },
                    # 人工審閱欄位（預留空白）
                    "human_review": {
                        "accuracy": None,       # 1–5
                        "fluency": None,        # 1–5
                        "terminology": None,    # 1–5
                        "comments": "",
                    },
                }
                model_results["translations"].append(entry)

                status = f" | {result.elapsed_sec:.1f}s"
                if metric.chrf is not None:
                    status += f" | ChrF {metric.chrf:.1f}"
                if metric.comet is not None:
                    status += f" | COMET {metric.comet:.3f}"
                if result.error:
                    status += f" | ERROR: {result.error[:40]}"
                print(status)

            total_time = time.perf_counter() - t_model_start
            backend.unload()

            # 計算摘要統計
            translations = model_results["translations"]
            has_ref = [t for t in translations if t["references"]]

            summary = {
                "total_time_sec": round(total_time, 1),
                "avg_elapsed_sec": round(
                    sum(t["elapsed_sec"] for t in translations) / len(translations), 2
                ) if translations else 0,
                "error_count": sum(1 for t in translations if t["error"]),
                "samples_with_reference": len(has_ref),
            }

            # 對每個參考來源各自算摘要
            if has_ref:
                ref_names = set()
                for t in has_ref:
                    ref_names.update(t["metrics_per_ref"].keys())

                summary["scores_per_ref"] = {}
                for ref_name in sorted(ref_names):
                    chrf_vals = [
                        t["metrics_per_ref"][ref_name]["chrf"]
                        for t in has_ref
                        if ref_name in t["metrics_per_ref"]
                        and t["metrics_per_ref"][ref_name]["chrf"] is not None
                    ]
                    comet_vals = [
                        t["metrics_per_ref"][ref_name]["comet"]
                        for t in has_ref
                        if ref_name in t["metrics_per_ref"]
                        and t["metrics_per_ref"][ref_name]["comet"] is not None
                    ]
                    summary["scores_per_ref"][ref_name] = {
                        "avg_chrf": round(sum(chrf_vals) / len(chrf_vals), 2) if chrf_vals else None,
                        "avg_comet": round(sum(comet_vals) / len(comet_vals), 4) if comet_vals else None,
                        "n": len(chrf_vals),
                    }

            model_results["summary"] = summary
            print(f"\n摘要：{summary}")

        except Exception as e:
            print(f"模型載入/評估失敗：{e}")
            model_results["fatal_error"] = str(e)

        all_results.append(model_results)

    output = {
        "meta": run_meta,
        "results": all_results,
    }

    Path(output_path).write_text(
        json.dumps(output, ensure_ascii=False, indent=2),
        encoding="utf-8"
    )
    print(f"\n結果已儲存到 {output_path}")
    return output


def main():
    parser = argparse.ArgumentParser(description="翻譯模型評估工具")
    parser.add_argument("--config", required=True, help="設定檔路徑（JSON）")
    parser.add_argument("--output", default="results.json", help="輸出檔案路徑")
    args = parser.parse_args()

    config = json.loads(Path(args.config).read_text(encoding="utf-8"))

    # 載入測試集
    samples: list[TestSample] = []

    if config.get("flores200"):
        f = config["flores200"]
        print(f"載入 FLORES-200：{f['source_lang']} → {f['target_lang']}")
        samples += load_flores200(
            source_lang=f["source_lang"],
            target_lang=f["target_lang"],
            max_samples=f.get("max_samples", 50),
        )

    if config.get("custom_samples"):
        print(f"載入自訂樣本：{config['custom_samples']}")
        samples += load_custom_samples(config["custom_samples"])

    print(f"共 {len(samples)} 筆測試樣本")

    run_evaluation(
        models=config["models"],
        samples=samples,
        output_path=args.output,
        use_comet=config.get("use_comet", True),
        comet_batch_size=config.get("comet_batch_size", 8),
    )


if __name__ == "__main__":
    main()

