import time
from pathlib import Path
from typing import Optional

from .base import BaseBackend, TranslationResult


class LlamaCppBackend(BaseBackend):
    """
    使用 llama-cpp-python 載入 GGUF 量化模型。
    model_id 應為 GGUF 檔案的本機路徑，例如：
        /models/gemma-3n-e4b-q4_k_m.gguf
    """

    def __init__(
        self,
        model_id: str,
        n_gpu_layers: int = -1,   # -1 = 全部層卸載到 GPU
        n_ctx: int = 4096,
        n_threads: Optional[int] = None,
        verbose: bool = False,
    ):
        super().__init__(model_id)
        self.n_gpu_layers = n_gpu_layers
        self.n_ctx = n_ctx
        self.n_threads = n_threads
        self.verbose = verbose
        self._llm = None

    def load(self) -> None:
        try:
            from llama_cpp import Llama
        except ImportError:
            raise ImportError("請先安裝 llama-cpp-python：pip install llama-cpp-python")

        path = Path(self.model_id)
        if not path.exists():
            raise FileNotFoundError(f"找不到 GGUF 檔案：{self.model_id}")

        self._llm = Llama(
            model_path=str(path),
            n_gpu_layers=self.n_gpu_layers,
            n_ctx=self.n_ctx,
            n_threads=self.n_threads,
            verbose=self.verbose,
        )
        self.model_name = path.stem

    def translate(self, text: str, source_lang: str, target_lang: str) -> TranslationResult:
        if self._llm is None:
            raise RuntimeError("模型尚未載入，請先呼叫 load()")

        prompt = self.build_prompt(text, source_lang, target_lang)
        t0 = time.perf_counter()

        try:
            response = self._llm(
                prompt,
                max_tokens=1024,
                temperature=0.0,
                stop=["Text:", "\n\n"],
            )
            elapsed = time.perf_counter() - t0
            output = response["choices"][0]["text"].strip()
            usage = response.get("usage", {})
            completion_tokens = usage.get("completion_tokens", 0)
            tps = completion_tokens / elapsed if elapsed > 0 else None

            return TranslationResult(
                source=text,
                translation=output,
                model_name=self.model_name,
                backend="llamacpp",
                elapsed_sec=round(elapsed, 3),
                tokens_per_sec=round(tps, 1) if tps else None,
            )
        except Exception as e:
            elapsed = time.perf_counter() - t0
            return TranslationResult(
                source=text,
                translation="",
                model_name=self.model_name,
                backend="llamacpp",
                elapsed_sec=round(elapsed, 3),
                error=str(e),
            )

    def unload(self) -> None:
        self._llm = None
