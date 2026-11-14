# Code editor examples

Read-only example files for every language the code editor registers. Open one
to try highlighting, folding and language-server features, or copy it into your
project (for example under `src/` or `scripts/`) and change it.

| Folder | Extension | Files |
|---|---|---|
| `rust/` | `.rs` | `inventory.rs` (structs, enums, traits, errors), `fibonacci.rs` (iterator, generics, tests) |
| `javascript/` | `.js` | `debounce.js` (closures, promises), `event_emitter.js` (class, private fields) |
| `typescript/` | `.ts` | `result.ts` (unions, generics, type guards), `state_machine.ts` (literal types, class) |
| `python/` | `.py` | `wordcount.py` (dataclass, typing), `grid_path.py` (BFS, generators) |
| `lua/` | `.lua` | `timers.lua` (closures, metatables), `vector.lua` (operator metamethods) |
| `toml/` | `.toml` | `project.toml` (tables, arrays of tables), `manifest.toml` (a Rust manifest) |
| `markdown/` | `.md` | `design_notes.md` (headings, tables, code, tasks), `changelog.md` |

These are general-purpose scripts. TypeScript classes for the engine's script
VM (`src/classes/<Class>/class.ts`) have their own examples in the TypeScript
plugin's folder.
