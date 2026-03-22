from abc import ABC, abstractmethod
from dataclasses import dataclass
from typing import Optional


@dataclass
class TranslationResult:
    source: str
    translation: str
    model_name: str
    backend: str
    elapsed_sec: float
    tokens_per_sec: Optional[float] = None
    error: Optional[str] = None


class BaseBackend(ABC):
    def __init__(self, model_id: str):
        self.model_id = model_id
        self.model_name = model_id.split("/")[-1]

    @abstractmethod
    def load(self) -> None:
        """載入模型到記憶體"""
        pass

    @abstractmethod
    def translate(self, text: str, source_lang: str, target_lang: str) -> TranslationResult:
        pass

    @abstractmethod
    def unload(self) -> None:
        """釋放記憶體"""
        pass

    def __enter__(self):
        self.load()
        return self

    def __exit__(self, *_):
        self.unload()

    @staticmethod
    def build_prompt(text: str, source_lang: str, target_lang: str) -> str:
        return (
            f"Translate the following text from {source_lang} to {target_lang}. "
            f"Output only the translation, nothing else.\n\n"
            f"Text: {text}\n\n"
            f"Translation:"
        )

