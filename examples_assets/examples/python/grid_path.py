"""Breadth-first search on a grid: classes, a deque and generators."""

from collections import deque
from typing import Iterator

# A map: '#' is a wall, '.' is floor, 'S' the start and 'G' the goal.
GRID = [
    "S..#....",
    ".#.#.##.",
    ".#...#..",
    ".####.#.",
    "......#G",
]


def find(grid: list[str], mark: str) -> tuple[int, int]:
    """The (row, column) of the first cell equal to `mark`."""
    for r, line in enumerate(grid):
        if mark in line:
            return r, line.index(mark)
    raise ValueError(f"no {mark!r} on the grid")


def neighbours(grid: list[str], cell: tuple[int, int]) -> Iterator[tuple[int, int]]:
    """The open cells next to `cell` (no diagonals)."""
    r, c = cell
    for dr, dc in ((1, 0), (-1, 0), (0, 1), (0, -1)):
        nr, nc = r + dr, c + dc
        if 0 <= nr < len(grid) and 0 <= nc < len(grid[nr]) and grid[nr][nc] != "#":
            yield nr, nc


def shortest_path(grid: list[str]) -> list[tuple[int, int]] | None:
    """The cells from S to G, or None when the goal cannot be reached."""
    start, goal = find(grid, "S"), find(grid, "G")
    came_from = {start: None}
    queue = deque([start])
    while queue:
        cell = queue.popleft()
        if cell == goal:
            path = []
            while cell is not None:
                path.append(cell)
                cell = came_from[cell]
            return path[::-1]
        for nxt in neighbours(grid, cell):
            if nxt not in came_from:
                came_from[nxt] = cell
                queue.append(nxt)
    return None


if __name__ == "__main__":
    path = shortest_path(GRID)
    print(f"{len(path) - 1} steps" if path else "no path")
