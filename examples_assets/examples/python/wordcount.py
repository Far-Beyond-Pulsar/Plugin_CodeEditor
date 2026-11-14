"""Count the most common words in a text: functions, comprehensions and typing."""

from collections import Counter
from dataclasses import dataclass
import re


@dataclass
class WordStat:
    """One word and how many times it appeared."""

    word: str
    count: int


def words(text: str) -> list[str]:
    """Lower-case words of `text`, ignoring punctuation."""
    return re.findall(r"[a-z']+", text.lower())


def most_common(text: str, limit: int = 3, min_length: int = 3) -> list[WordStat]:
    """The `limit` most frequent words of at least `min_length` letters."""
    counts = Counter(w for w in words(text) if len(w) >= min_length)
    return [WordStat(word, count) for word, count in counts.most_common(limit)]


if __name__ == "__main__":
    sample = "The quick fox jumps over the lazy dog. The dog sleeps; the fox runs."
    for stat in most_common(sample):
        print(f"{stat.word:<8} {stat.count}")
