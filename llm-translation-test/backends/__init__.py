from pathlib import Path

from .base import BaseBackend
from .llamacpp_backend import LlamaCppBackend
from .transformers_backend import TransformersBackend


def auto_backend(model_id: str, **kwargs) -> BaseBackend:
    """
    根據 model_id 自動選擇後端：
    - 副檔名為 .gguf → LlamaCppBackend
    - 其他（HF repo 或本機資料夾）→ TransformersBackend
    """
    if Path(model_id).suffix.lower() == ".gguf":
        llamacpp_kwargs = {k: v for k, v in kwargs.items()
                          if k in ("n_gpu_layers", "n_ctx", "n_threads", "verbose")}
        return LlamaCppBackend(model_id, **llamacpp_kwargs)
    else:
        transformers_kwargs = {k: v for k, v in kwargs.items()
                               if k in ("dtype", "device_map", "max_new_tokens",
                                        "load_in_4bit", "load_in_8bit")}
        return TransformersBackend(model_id, **transformers_kwargs)


__all__ = ["auto_backend", "BaseBackend", "LlamaCppBackend", "TransformersBackend"]

