import time
from typing import Optional

import torch

from .base import BaseBackend, TranslationResult


class TransformersBackend(BaseBackend):
    """
    使用 HuggingFace transformers 載入模型。
    model_id 可以是 HF repo（如 google/gemma-3n-E4B-it）
    或本機路徑。
    """

    def __init__(
        self,
        model_id: str,
        dtype: str = "bfloat16",       # bfloat16 在 4090 上最穩
        device_map: str = "auto",
        max_new_tokens: int = 1024,
        load_in_4bit: bool = False,     # 若 VRAM 不足可開啟
        load_in_8bit: bool = False,
    ):
        super().__init__(model_id)
        self.dtype = getattr(torch, dtype)
        self.device_map = device_map
        self.max_new_tokens = max_new_tokens
        self.load_in_4bit = load_in_4bit
        self.load_in_8bit = load_in_8bit
        self._model = None
        self._tokenizer = None

    def load(self) -> None:
        try:
            from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig
        except ImportError:
            raise ImportError("請先安裝 transformers：pip install transformers accelerate")

        quantization_config = None
        if self.load_in_4bit or self.load_in_8bit:
            quantization_config = BitsAndBytesConfig(
                load_in_4bit=self.load_in_4bit,
                load_in_8bit=self.load_in_8bit,
                bnb_4bit_compute_dtype=self.dtype,
            )

        self._tokenizer = AutoTokenizer.from_pretrained(self.model_id)
        self._model = AutoModelForCausalLM.from_pretrained(
            self.model_id,
            torch_dtype=self.dtype,
            device_map=self.device_map,
            quantization_config=quantization_config,
        )
        self._model.eval()

    def translate(self, text: str, source_lang: str, target_lang: str) -> TranslationResult:
        if self._model is None or self._tokenizer is None:
            raise RuntimeError("模型尚未載入，請先呼叫 load()")

        prompt = self.build_prompt(text, source_lang, target_lang)

        # 嘗試使用 chat template（如果模型支援）
        if hasattr(self._tokenizer, "chat_template") and self._tokenizer.chat_template:
            messages = [{"role": "user", "content": prompt}]
            formatted = self._tokenizer.apply_chat_template(
                messages, tokenize=False, add_generation_prompt=True
            )
        else:
            formatted = prompt

        inputs = self._tokenizer(formatted, return_tensors="pt").to(self._model.device)
        input_len = inputs["input_ids"].shape[-1]

        t0 = time.perf_counter()
        try:
            with torch.inference_mode():
                outputs = self._model.generate(
                    **inputs,
                    max_new_tokens=self.max_new_tokens,
                    do_sample=False,
                    temperature=None,
                    top_p=None,
                    pad_token_id=self._tokenizer.eos_token_id,
                )
            elapsed = time.perf_counter() - t0
            new_tokens = outputs[0][input_len:]
            output = self._tokenizer.decode(new_tokens, skip_special_tokens=True).strip()
            tps = len(new_tokens) / elapsed if elapsed > 0 else None

            return TranslationResult(
                source=text,
                translation=output,
                model_name=self.model_name,
                backend="transformers",
                elapsed_sec=round(elapsed, 3),
                tokens_per_sec=round(tps, 1) if tps else None,
            )
        except Exception as e:
            elapsed = time.perf_counter() - t0
            return TranslationResult(
                source=text,
                translation="",
                model_name=self.model_name,
                backend="transformers",
                elapsed_sec=round(elapsed, 3),
                error=str(e),
            )

    def unload(self) -> None:
        import gc
        self._model = None
        self._tokenizer = None
        gc.collect()
        if torch.cuda.is_available():
            torch.cuda.empty_cache()
