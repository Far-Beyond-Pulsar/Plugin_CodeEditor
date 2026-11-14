// Types: a discriminated union, generics and a type guard.

/** The outcome of something that can fail, without exceptions. */
type Result<T, E> = { ok: true; value: T } | { ok: false; error: E };

function ok<T>(value: T): Result<T, never> {
  return { ok: true, value };
}

function fail<E>(error: E): Result<never, E> {
  return { ok: false, error };
}

/** Transforms the value of a successful result. */
function map<T, U, E>(result: Result<T, E>, change: (value: T) => U): Result<U, E> {
  return result.ok ? ok(change(result.value)) : result;
}

interface Settings {
  volume: number;
  fullscreen: boolean;
}

/** Parses a settings object from JSON text. */
function parseSettings(text: string): Result<Settings, string> {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return fail("not valid JSON");
  }
  if (!isSettings(raw)) {
    return fail("missing volume or fullscreen");
  }
  return ok(raw);
}

/** A type guard: narrows `value` to Settings when it returns true. */
function isSettings(value: unknown): value is Settings {
  const v = value as Partial<Settings>;
  return typeof v?.volume === "number" && typeof v?.fullscreen === "boolean";
}

const parsed = map(parseSettings('{"volume": 0.8, "fullscreen": true}'), (s) => s.volume * 100);
console.log(parsed.ok ? `volume ${parsed.value}%` : `error: ${parsed.error}`);
